use anyhow::{Context, Result};
use image::RgbImage;
use std::io::Cursor;

/// Report ID for LCD image output (0x1A10 interface).
const REPORT_ID: u8 = 0x14;

/// Full HID report size (report ID + 4094 bytes payload).
const PACKET_SIZE: usize = 4095;

/// Header size for the first packet.
const FIRST_HEADER_LEN: usize = 20;

/// Header size for continuation packets.
const CONT_HEADER_LEN: usize = 5;

/// Button pixel size (square).
pub const BUTTON_PX: u16 = 118;

/// JPEG encoding quality (0-100).
const JPEG_QUALITY: u8 = 95;

/// Pixel positions for each LCD button (0-indexed) on the 480x480 panel.
/// Button index 0 = top-left, 8 = bottom-right (left-to-right, top-to-bottom).
const BUTTON_POSITIONS: [(u16, u16); 9] = [
    (23, 6),    // button 0 (config id 1)
    (181, 6),   // button 1 (config id 2)
    (339, 6),   // button 2 (config id 3)
    (23, 164),  // button 3 (config id 4)
    (181, 164), // button 4 (config id 5)
    (339, 164), // button 5 (config id 6)
    (23, 322),  // button 6 (config id 7)
    (181, 322), // button 7 (config id 8)
    (339, 322), // button 8 (config id 9)
];

/// Get the pixel position for a button by config ID (1-9).
fn button_position(config_id: u8) -> Option<(u16, u16)> {
    if config_id >= 1 && config_id <= 9 {
        Some(BUTTON_POSITIONS[(config_id - 1) as usize])
    } else {
        None
    }
}

/// Encode an RGB image as JPEG bytes.
fn encode_jpeg(img: &RgbImage) -> Result<Vec<u8>> {
    let mut buf = Cursor::new(Vec::new());
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, JPEG_QUALITY);
    img.write_with_encoder(encoder)
        .context("Failed to encode JPEG")?;
    Ok(buf.into_inner())
}

/// Generate the packet control byte.
fn packet_control(index: u8, is_first: bool, is_last: bool) -> u8 {
    let mut value = index | 0b0010_0000;
    if is_first {
        value |= 0b1000_0000;
    }
    if is_last {
        value |= 0b0100_0000;
    }
    value
}

/// Build HID output packets for writing a JPEG image at pixel coordinates (x, y, w, h).
fn build_image_packets(x: u16, y: u16, w: u16, h: u16, jpeg: &[u8]) -> Vec<[u8; PACKET_SIZE]> {
    let mut packets = Vec::new();

    // First packet
    let first_payload = (jpeg.len()).min(PACKET_SIZE - FIRST_HEADER_LEN);
    let mut p = [0u8; PACKET_SIZE];
    p[0] = REPORT_ID;
    p[1] = 0xFF;
    p[2] = 0x02;
    p[3] = 0x2B;
    p[4] = packet_control(1, true, first_payload >= jpeg.len());
    // Bytes 5-8: fixed 0x0100, 0x0100
    p[5] = 0x01;
    p[6] = 0x00;
    p[7] = 0x01;
    p[8] = 0x00;
    // Pixel coordinates (big-endian u16)
    p[9..11].copy_from_slice(&x.to_be_bytes());
    p[11..13].copy_from_slice(&y.to_be_bytes());
    p[13..15].copy_from_slice(&w.to_be_bytes());
    p[15..17].copy_from_slice(&h.to_be_bytes());
    // p[17] = 0x00 (padding)
    // Total JPEG length (big-endian u16)
    p[18..20].copy_from_slice(&(jpeg.len() as u16).to_be_bytes());
    // JPEG data
    p[FIRST_HEADER_LEN..FIRST_HEADER_LEN + first_payload].copy_from_slice(&jpeg[..first_payload]);
    packets.push(p);

    // Continuation packets
    let mut offset = first_payload;
    let mut part: u8 = 2;
    while offset < jpeg.len() {
        let payload = (jpeg.len() - offset).min(PACKET_SIZE - CONT_HEADER_LEN);
        let mut pn = [0u8; PACKET_SIZE];
        pn[0] = REPORT_ID;
        pn[1] = 0xFF;
        pn[2] = 0x02;
        pn[3] = 0x2B;
        pn[4] = packet_control(part, false, offset + payload >= jpeg.len());
        pn[CONT_HEADER_LEN..CONT_HEADER_LEN + payload]
            .copy_from_slice(&jpeg[offset..offset + payload]);
        packets.push(pn);
        offset += payload;
        part += 1;
    }

    packets
}

/// Create a solid color image for a button.
pub fn solid_color_image(r: u8, g: u8, b: u8) -> RgbImage {
    RgbImage::from_fn(BUTTON_PX as u32, BUTTON_PX as u32, |_, _| {
        image::Rgb([r, g, b])
    })
}

/// Create a text label image at a custom size (for Stream Deck compatibility).
pub fn label_image_sized(text: &str, fg: [u8; 3], bg: [u8; 3], width: u32, height: u32, font_scale: Option<u32>) -> RgbImage {
    label_image_impl(text, fg, bg, width, height, font_scale)
}

/// Create a text label image for a button (scaled bitmap font).
/// Uses a basic built-in 5x7 bitmap font scaled up to fill the 118x118 LCD.
pub fn label_image(text: &str, fg: [u8; 3], bg: [u8; 3]) -> RgbImage {
    label_image_impl(text, fg, bg, BUTTON_PX as u32, BUTTON_PX as u32, None)
}

fn label_image_impl(text: &str, fg: [u8; 3], bg: [u8; 3], width: u32, height: u32, font_scale: Option<u32>) -> RgbImage {
    let mut img = RgbImage::from_fn(width, height, |_, _| image::Rgb(bg));

    let lines: Vec<&str> = text.split('\n').collect();
    if lines.is_empty() || (lines.len() == 1 && lines[0].is_empty()) {
        return img;
    }

    let glyph_w = 5u32;
    let glyph_h = 7u32;
    let spacing = 1u32;
    let line_spacing = 2u32;
    let num_lines = lines.len() as u32;

    // Find the longest line to determine scale
    let max_chars = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0) as u32;
    if max_chars == 0 {
        return img;
    }
    let max_glyphs_w = max_chars * (glyph_w + spacing) - spacing;

    let margin_w = width / 10;
    let margin_h = height / 10;
    let available_w = width - 2 * margin_w;
    let available_h = height - 2 * margin_h;
    let scale = if let Some(fs) = font_scale {
        fs.max(1)
    } else {
        let scale_x = available_w / max_glyphs_w;
        let total_glyph_h = num_lines * glyph_h + (num_lines - 1) * line_spacing;
        let scale_y = available_h / total_glyph_h;
        scale_x.min(scale_y).max(1)
    };

    let char_w = (glyph_w + spacing) * scale;
    let row_h = glyph_h * scale + line_spacing * scale;
    let total_h = num_lines * glyph_h * scale + (num_lines - 1) * line_spacing * scale;
    let block_start_y = height.saturating_sub(total_h) / 2;

    for (li, line) in lines.iter().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        if chars.is_empty() {
            continue;
        }
        let line_w = chars.len() as u32 * char_w - spacing * scale;
        let start_x = width.saturating_sub(line_w) / 2;
        let start_y = block_start_y + li as u32 * row_h;

        for (ci, &ch) in chars.iter().enumerate() {
            let glyph = get_glyph(ch);
            for (row, &bits) in glyph.iter().enumerate() {
                for col in 0..glyph_w {
                    if bits & (1 << (4 - col)) != 0 {
                        for dy in 0..scale {
                            for dx in 0..scale {
                                let px = start_x + ci as u32 * char_w + col * scale + dx;
                                let py = start_y + row as u32 * scale + dy;
                                if px < width && py < height {
                                    img.put_pixel(px, py, image::Rgb(fg));
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    img
}

/// Get a 5x7 bitmap glyph for a character.
fn get_glyph(ch: char) -> [u8; 7] {
    match ch {
        'A' => [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'B' => [0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E],
        'C' => [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E],
        'D' => [0x1E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1E],
        'E' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F],
        'F' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10],
        'G' => [0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0E],
        'H' => [0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'I' => [0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E],
        'J' => [0x07, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0C],
        'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F],
        'M' => [0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x19, 0x15, 0x13, 0x11, 0x11, 0x11],
        'O' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'P' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10],
        'Q' => [0x0E, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0D],
        'R' => [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11],
        'S' => [0x0E, 0x11, 0x10, 0x0E, 0x01, 0x11, 0x0E],
        'T' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04],
        'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x1B, 0x11],
        'X' => [0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11],
        'Y' => [0x11, 0x11, 0x0A, 0x04, 0x04, 0x04, 0x04],
        'Z' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1F],
        '0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        '1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        '2' => [0x0E, 0x11, 0x01, 0x06, 0x08, 0x10, 0x1F],
        '3' => [0x0E, 0x11, 0x01, 0x06, 0x01, 0x11, 0x0E],
        '4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        '5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        '6' => [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
        '7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        '9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
        ' ' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
        '/' => [0x01, 0x02, 0x02, 0x04, 0x08, 0x08, 0x10],
        ':' => [0x00, 0x04, 0x04, 0x00, 0x04, 0x04, 0x00],
        '-' => [0x00, 0x00, 0x00, 0x1F, 0x00, 0x00, 0x00],
        '_' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1F],
        '.' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04],
        '>' => [0x10, 0x08, 0x04, 0x02, 0x04, 0x08, 0x10],
        '<' => [0x01, 0x02, 0x04, 0x08, 0x04, 0x02, 0x01],
        // Lowercase maps to uppercase
        'a'..='z' => get_glyph(ch.to_ascii_uppercase()),
        _ => [0x1F, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1F], // box for unknown
    }
}

/// LCD writer that sends images to the device's LCD button screens.
pub struct LcdWriter {
    device: hidapi::HidDevice,
}

impl LcdWriter {
    /// Open the LCD output interface (usage 0x1A10).
    pub fn open(api: &hidapi::HidApi, vid: u16, pid: u16, usage_page: u16) -> Result<Self> {
        let info = api
            .device_list()
            .find(|d| {
                d.vendor_id() == vid
                    && d.product_id() == pid
                    && d.usage_page() == usage_page
                    && d.usage() == 0x1A10
            })
            .context("LCD output interface (usage=0x1A10) not found")?;

        let device = api
            .open_path(info.path())
            .context("Failed to open LCD output interface")?;

        tracing::info!("Opened LCD output interface (usage=0x1A10)");
        Ok(Self { device })
    }

    /// Write an RGB image to a specific LCD button (config ID 1-9).
    pub fn write_button_image(&self, config_id: u8, img: &RgbImage) -> Result<()> {
        let (x, y) = button_position(config_id)
            .with_context(|| format!("Invalid LCD button config_id: {config_id}"))?;

        // Resize if needed
        let resized = if img.width() != BUTTON_PX as u32 || img.height() != BUTTON_PX as u32 {
            image::imageops::resize(
                img,
                BUTTON_PX as u32,
                BUTTON_PX as u32,
                image::imageops::FilterType::Triangle,
            )
            .into()
        } else {
            img.clone()
        };

        let jpeg = encode_jpeg(&resized)?;
        let packets = build_image_packets(x, y, BUTTON_PX, BUTTON_PX, &jpeg);

        tracing::debug!(
            config_id,
            jpeg_len = jpeg.len(),
            packets = packets.len(),
            "Writing LCD button image"
        );

        for packet in &packets {
            self.device
                .write(packet)
                .with_context(|| format!("Failed to write LCD image packet for button {config_id}"))?;
        }

        Ok(())
    }

    /// Write a text label to a specific LCD button.
    pub fn write_button_label(
        &self,
        config_id: u8,
        text: &str,
        fg: [u8; 3],
        bg: [u8; 3],
    ) -> Result<()> {
        let img = label_image(text, fg, bg);
        self.write_button_image(config_id, &img)
    }

    /// Load an image file and write it to a button.
    pub fn write_button_file(&self, config_id: u8, path: &std::path::Path) -> Result<()> {
        let img = image::open(path)
            .with_context(|| format!("Failed to load image: {}", path.display()))?
            .into_rgb8();
        self.write_button_image(config_id, &img)
    }

    /// Clear a button (set to black).
    pub fn clear_button(&self, config_id: u8) -> Result<()> {
        let img = solid_color_image(0, 0, 0);
        self.write_button_image(config_id, &img)
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_control_single() {
        assert_eq!(packet_control(1, true, true), 0xE1);
    }

    #[test]
    fn packet_control_first_of_multi() {
        assert_eq!(packet_control(1, true, false), 0xA1);
    }

    #[test]
    fn packet_control_middle() {
        assert_eq!(packet_control(2, false, false), 0x22);
    }

    #[test]
    fn packet_control_last() {
        assert_eq!(packet_control(3, false, true), 0x63);
    }

    #[test]
    fn build_single_packet_image() {
        // Small JPEG-like data that fits in one packet
        let jpeg = vec![0xFFu8; 100];
        let packets = build_image_packets(23, 6, 118, 118, &jpeg);
        assert_eq!(packets.len(), 1);
        assert_eq!(packets[0][0], 0x14); // report ID
        assert_eq!(packets[0][3], 0x2B); // command
        assert_eq!(packets[0][4], 0xE1); // single packet
        // Check coordinates
        assert_eq!(&packets[0][9..11], &23u16.to_be_bytes());
        assert_eq!(&packets[0][11..13], &6u16.to_be_bytes());
        // Check total length
        assert_eq!(&packets[0][18..20], &100u16.to_be_bytes());
    }

    #[test]
    fn build_multi_packet_image() {
        // Data that needs 2 packets
        let jpeg = vec![0xAAu8; 5000];
        let packets = build_image_packets(23, 6, 118, 118, &jpeg);
        assert_eq!(packets.len(), 2);
        assert_eq!(packets[0][4], 0xA1); // first, not last
        assert_eq!(packets[1][4], 0x62); // last, index=2
    }

    #[test]
    fn button_positions_valid() {
        for id in 1..=9 {
            assert!(button_position(id).is_some());
        }
        assert!(button_position(0).is_none());
        assert!(button_position(10).is_none());
    }

    #[test]
    fn label_image_creates_correct_size() {
        let img = label_image("TEST", [255, 255, 255], [0, 0, 0]);
        assert_eq!(img.width(), BUTTON_PX as u32);
        assert_eq!(img.height(), BUTTON_PX as u32);
    }

    #[test]
    fn solid_color_creates_correct_size() {
        let img = solid_color_image(255, 0, 0);
        assert_eq!(img.width(), BUTTON_PX as u32);
        assert_eq!(img.height(), BUTTON_PX as u32);
    }
}
