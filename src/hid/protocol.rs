use std::collections::HashSet;
use std::fmt;

/// Button identifier on the MX Creative Console Keypad.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ButtonId {
    /// LCD button 1-9 (3x3 grid, left-to-right, top-to-bottom)
    Lcd(u8),
    /// Left/back page arrow (HID++ hidId 0x01A1)
    PageLeft,
    /// Right/forward page arrow (HID++ hidId 0x01A2)
    PageRight,
}

impl ButtonId {
    /// Convert a button ID to the config-facing numeric ID (1-11).
    pub fn to_config_id(self) -> u8 {
        match self {
            ButtonId::Lcd(n) => n,
            ButtonId::PageLeft => 10,
            ButtonId::PageRight => 11,
        }
    }
}

impl fmt::Display for ButtonId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ButtonId::Lcd(n) => write!(f, "LCD{n}"),
            ButtonId::PageLeft => write!(f, "PageLeft"),
            ButtonId::PageRight => write!(f, "PageRight"),
        }
    }
}

/// A button event (press or release).
#[derive(Debug, Clone)]
pub enum ButtonEvent {
    Down(ButtonId),
    Up(ButtonId),
}

/// Parses HID reports from the MX Creative Console Keypad and
/// synthesizes Down/Up events by tracking pressed state.
pub struct ReportParser {
    pressed: HashSet<ButtonId>,
}

impl ReportParser {
    pub fn new() -> Self {
        Self {
            pressed: HashSet::new(),
        }
    }

    /// Parse a raw HID report buffer and return synthesized button events.
    ///
    /// Buffer layout (Windows): byte 0 = report ID.
    ///
    /// LCD Button Report (reportId = 0x13):
    ///   buffer[0] = 0x13  (report ID)
    ///   buffer[1] = 0xFF  (constant)
    ///   buffer[2] = 0x02  (constant)
    ///   buffer[3] = 0x00  (constant)
    ///   buffer[4] = ??    (reserved/unknown)
    ///   buffer[5] = 0x01  (button state report indicator)
    ///   buffer[6..] = pressed hidIds (1-byte each, 0x01-0x09), terminated by 0x00
    ///
    /// Page Button Report (reportId = 0x11):
    ///   buffer[0] = 0x11  (report ID)
    ///   buffer[1] = 0xFF  (constant)
    ///   buffer[2] = 0x0B  (constant)
    ///   buffer[3] = 0x00  (constant)
    ///   buffer[4..] = pressed hidIds (2-byte big-endian each), terminated by 0x0000
    ///                 0x01A1 = PageLeft, 0x01A2 = PageRight
    pub fn parse(&mut self, buf: &[u8]) -> Vec<ButtonEvent> {
        if buf.is_empty() {
            return vec![];
        }

        let report_id = buf[0];
        match report_id {
            0x13 => self.parse_lcd_report(buf),
            0x11 => self.parse_page_report(buf),
            _ => {
                tracing::debug!(report_id = format!("0x{:02X}", report_id), len = buf.len(),
                    bytes = %format_hex(buf), "Unknown HID report");
                vec![]
            }
        }
    }

    fn parse_lcd_report(&mut self, buf: &[u8]) -> Vec<ButtonEvent> {
        // Minimum valid LCD report: reportId + 3 constants + reserved + indicator = 6 bytes
        if buf.len() < 6 {
            tracing::debug!(len = buf.len(), "LCD report too short");
            return vec![];
        }

        // Verify indicator byte at buffer[5]
        // 0x01 = button state report; 0x00 = LCD write ack (ignore silently)
        if buf[5] != 0x01 {
            tracing::trace!(indicator = buf[5], "LCD report: non-button report (ack)");
            return vec![];
        }

        // Extract currently pressed LCD button IDs from buffer[6..]
        // CIDs are 0x5E-0x65 for the 9 LCD buttons, mapped to button numbers 1-9.
        // Also accept raw 1-9 in case firmware sends those directly.
        let mut currently_pressed = HashSet::new();
        for &byte in buf.get(6..).unwrap_or(&[]) {
            if byte == 0x00 {
                break;
            }
            if let Some(btn_num) = cid_to_lcd_button(byte as u16) {
                currently_pressed.insert(ButtonId::Lcd(btn_num));
            } else if (1..=9).contains(&byte) {
                currently_pressed.insert(ButtonId::Lcd(byte));
            } else {
                tracing::debug!(hid_id = format!("0x{byte:02X}"), "LCD report: unexpected button hidId");
            }
        }

        self.diff_events(&currently_pressed, 0x13)
    }

    fn parse_page_report(&mut self, buf: &[u8]) -> Vec<ButtonEvent> {
        // Minimum valid page report: reportId + 3 constants = 4 bytes
        if buf.len() < 4 {
            tracing::debug!(len = buf.len(), "Page report too short");
            return vec![];
        }

        // Filter out non-button reports:
        // - Heartbeat/keepalive: buf[2] == 0x04 (feature 0x041C battery status)
        // - Ack responses: buf[3] has function nibble matching setCidReporting acks (0x2B or 0x3B)
        if buf.len() >= 3 && buf[2] == 0x04 {
            tracing::trace!("Ignoring heartbeat report");
            return vec![];
        }
        if buf.len() >= 4 && (buf[3] == 0x2B || buf[3] == 0x3B) {
            tracing::trace!("Ignoring setCidReporting ack");
            return vec![];
        }

        // Diverted button events: buf[1]=0xFF, buf[2]=featureIdx, buf[3]=function|swId
        // The button CIDs are 2-byte BE starting at buf[4]
        let mut currently_pressed = HashSet::new();
        let data = buf.get(4..).unwrap_or(&[]);
        let mut i = 0;
        while i + 1 < data.len() {
            let hid_id = u16::from_be_bytes([data[i], data[i + 1]]);
            if hid_id == 0x0000 {
                break;
            }
            match hid_id {
                0x01A1 => {
                    currently_pressed.insert(ButtonId::PageLeft);
                }
                0x01A2 => {
                    currently_pressed.insert(ButtonId::PageRight);
                }
                cid if cid_to_lcd_button(cid).is_some() => {
                    currently_pressed.insert(ButtonId::Lcd(cid_to_lcd_button(cid).unwrap()));
                }
                _ => {
                    tracing::debug!(hid_id = format!("0x{hid_id:04X}"), "Page report: unexpected button CID");
                }
            }
            i += 2;
        }

        self.diff_events(&currently_pressed, 0x11)
    }

    /// Diff the currently pressed set against the tracked state to synthesize Down/Up events.
    /// `report_id` is used to scope release detection: only buttons that could appear
    /// in this report type are candidates for Up events.
    fn diff_events(&mut self, currently_pressed: &HashSet<ButtonId>, report_id: u8) -> Vec<ButtonEvent> {
        let mut events = Vec::new();

        // Buttons that are in currently_pressed but not in self.pressed -> Down
        for &btn in currently_pressed {
            if !self.pressed.contains(&btn) {
                events.push(ButtonEvent::Down(btn));
            }
        }

        // Buttons that are in self.pressed but not in currently_pressed -> Up
        // Only consider buttons that could appear on this report_id to avoid cross-interference.
        // 0x11: all diverted buttons (arrow + LCD); 0x13: only LCD buttons
        let released: Vec<ButtonId> = self
            .pressed
            .iter()
            .filter(|btn| {
                let could_be_on_this_report = match report_id {
                    0x11 => true, // diverted buttons: all types
                    0x13 => matches!(btn, ButtonId::Lcd(_)), // LCD-only report
                    _ => false,
                };
                could_be_on_this_report && !currently_pressed.contains(btn)
            })
            .copied()
            .collect();

        for btn in released {
            self.pressed.remove(&btn);
            events.push(ButtonEvent::Up(btn));
        }

        // Add newly pressed buttons to tracked state
        for &btn in currently_pressed {
            self.pressed.insert(btn);
        }

        events
    }
}

/// Map an LCD button CID (0x5E-0x65) to a button number (1-8).
/// Returns None if the CID is not a recognized LCD button.
fn cid_to_lcd_button(cid: u16) -> Option<u8> {
    if cid >= 0x5E && cid <= 0x65 {
        Some((cid - 0x5E + 1) as u8)
    } else {
        None
    }
}

/// Format a byte slice as hex string for logging.
pub fn format_hex(buf: &[u8]) -> String {
    buf.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_lcd_button_press() {
        let mut parser = ReportParser::new();
        // LCD button 1 pressed: reportId=0x13, constants, indicator=0x01, hidId=0x01, terminator
        let buf = [0x13, 0xFF, 0x02, 0x00, 0x00, 0x01, 0x01, 0x00];
        let events = parser.parse(&buf);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], ButtonEvent::Down(ButtonId::Lcd(1))));
    }

    #[test]
    fn parse_lcd_button_release() {
        let mut parser = ReportParser::new();
        // Press button 1
        let buf_press = [0x13, 0xFF, 0x02, 0x00, 0x00, 0x01, 0x01, 0x00];
        parser.parse(&buf_press);

        // Release (empty pressed set)
        let buf_release = [0x13, 0xFF, 0x02, 0x00, 0x00, 0x01, 0x00];
        let events = parser.parse(&buf_release);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], ButtonEvent::Up(ButtonId::Lcd(1))));
    }

    #[test]
    fn parse_lcd_multi_press() {
        let mut parser = ReportParser::new();
        // Buttons 1 and 3 pressed simultaneously
        let buf = [0x13, 0xFF, 0x02, 0x00, 0x00, 0x01, 0x01, 0x03, 0x00];
        let events = parser.parse(&buf);
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn parse_page_left_press() {
        let mut parser = ReportParser::new();
        // Page left (0x01A1) pressed
        let buf = [0x11, 0xFF, 0x0B, 0x00, 0x01, 0xA1, 0x00, 0x00];
        let events = parser.parse(&buf);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], ButtonEvent::Down(ButtonId::PageLeft)));
    }

    #[test]
    fn parse_page_right_press() {
        let mut parser = ReportParser::new();
        // Page right (0x01A2) pressed
        let buf = [0x11, 0xFF, 0x0B, 0x00, 0x01, 0xA2, 0x00, 0x00];
        let events = parser.parse(&buf);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], ButtonEvent::Down(ButtonId::PageRight)));
    }

    #[test]
    fn parse_page_release() {
        let mut parser = ReportParser::new();
        // Press page left
        let buf_press = [0x11, 0xFF, 0x0B, 0x00, 0x01, 0xA1, 0x00, 0x00];
        parser.parse(&buf_press);

        // Release (terminator only)
        let buf_release = [0x11, 0xFF, 0x0B, 0x00, 0x00, 0x00];
        let events = parser.parse(&buf_release);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], ButtonEvent::Up(ButtonId::PageLeft)));
    }

    #[test]
    fn unknown_report_ignored() {
        let mut parser = ReportParser::new();
        let buf = [0x20, 0x01, 0x02, 0x03];
        let events = parser.parse(&buf);
        assert!(events.is_empty());
    }

    #[test]
    fn empty_buffer_ignored() {
        let mut parser = ReportParser::new();
        let events = parser.parse(&[]);
        assert!(events.is_empty());
    }

    #[test]
    fn button_id_to_config_id() {
        assert_eq!(ButtonId::Lcd(1).to_config_id(), 1);
        assert_eq!(ButtonId::Lcd(9).to_config_id(), 9);
        assert_eq!(ButtonId::PageLeft.to_config_id(), 10);
        assert_eq!(ButtonId::PageRight.to_config_id(), 11);
    }
}
