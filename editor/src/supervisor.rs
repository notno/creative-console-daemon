//! Supervises the daemon child process: spawn, stop, restart, watchdog.
//!
//! Lives in the resident tray process. Daemon stdout/stderr is tee'd to a log
//! file so the (separate) editor process can tail it. Auto-restarts on exit
//! code 2 (device disconnect), the same as the old tray.ps1.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, SetInformationJobObject,
    JobObjectExtendedLimitInformation, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

/// Windows CREATE_NO_WINDOW — keep the daemon child fully windowless.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Create a Job Object configured to kill all member processes when the last
/// handle closes. The resident process holds this handle, so if it dies — even
/// force-killed via Task Manager — Windows closes the handle and terminates the
/// daemon child, preventing an orphaned daemon from holding the HID device.
fn create_kill_on_close_job() -> Option<HANDLE> {
    unsafe {
        let job = CreateJobObjectW(None, PCWSTR::null()).ok()?;
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
        .is_err()
        {
            let _ = CloseHandle(job);
            return None;
        }
        Some(job)
    }
}

/// Add a freshly-spawned child to the job. Failure only loses the kill-on-close
/// guarantee (e.g. if the child is already in an incompatible job), so it's
/// non-fatal.
fn assign_child_to_job(job: HANDLE, child: &Child) {
    unsafe {
        let _ = AssignProcessToJobObject(job, HANDLE(child.as_raw_handle()));
    }
}

type LogSink = Option<Arc<Mutex<File>>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    NotStarted,
    Running { pid: u32 },
    Restarting,
    Stopped { code: Option<i32> },
}

impl Status {
    pub fn label(&self) -> String {
        match self {
            Status::NotStarted => "not started".into(),
            Status::Running { pid } => format!("running (PID {pid})"),
            Status::Restarting => "restarting…".into(),
            Status::Stopped { code: Some(c) } => format!("stopped (exit {c})"),
            Status::Stopped { code: None } => "stopped".into(),
        }
    }
}

pub struct Supervisor {
    exe: PathBuf,
    config: PathBuf,
    log: LogSink,
    child: Option<Child>,
    /// Kills the daemon child if this process dies (see `create_kill_on_close_job`).
    job: Option<HANDLE>,
    /// Set when the user explicitly stopped the daemon, suppressing auto-restart.
    user_stopped: bool,
    pub status: Status,
}

impl Supervisor {
    pub fn new(exe: PathBuf, config: PathBuf, log_path: &Path) -> Self {
        let log = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(log_path)
            .ok()
            .map(|f| Arc::new(Mutex::new(f)));
        Self {
            exe,
            config,
            log,
            child: None,
            job: create_kill_on_close_job(),
            user_stopped: false,
            status: Status::NotStarted,
        }
    }

    fn write_log(log: &LogSink, line: &str) {
        if let Some(l) = log {
            if let Ok(mut f) = l.lock() {
                let _ = writeln!(f, "{line}");
                let _ = f.flush();
            }
        }
    }

    /// Spawn the daemon child. No-op if already running.
    pub fn start(&mut self) {
        if self.child.as_mut().is_some_and(|c| matches!(c.try_wait(), Ok(None))) {
            return;
        }
        self.user_stopped = false;

        let mut cmd = Command::new(&self.exe);
        cmd.arg("--config")
            .arg(&self.config)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        match cmd.spawn() {
            Ok(mut child) => {
                let pid = child.id();
                Self::write_log(&self.log, &format!("[supervisor] started daemon (PID {pid})"));

                if let Some(job) = self.job {
                    assign_child_to_job(job, &child);
                }

                if let Some(out) = child.stdout.take() {
                    let log = self.log.clone();
                    thread::spawn(move || {
                        for line in BufReader::new(out).lines().map_while(Result::ok) {
                            Self::write_log(&log, &line);
                        }
                    });
                }
                if let Some(err) = child.stderr.take() {
                    let log = self.log.clone();
                    thread::spawn(move || {
                        for line in BufReader::new(err).lines().map_while(Result::ok) {
                            Self::write_log(&log, &line);
                        }
                    });
                }

                self.child = Some(child);
                self.status = Status::Running { pid };
            }
            Err(e) => {
                Self::write_log(&self.log, &format!("[supervisor] failed to start daemon: {e}"));
                self.status = Status::Stopped { code: None };
            }
        }
    }

    /// Stop the daemon and suppress auto-restart.
    pub fn stop(&mut self) {
        self.user_stopped = true;
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.child = None;
        self.status = Status::Stopped { code: None };
        Self::write_log(&self.log, "[supervisor] daemon stopped by user");
    }

    /// Stop then start.
    pub fn restart(&mut self) {
        Self::write_log(&self.log, "[supervisor] restarting daemon…");
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.child = None;
        self.start();
    }

    /// Poll child liveness; auto-restart on exit code 2 (device disconnect).
    pub fn poll(&mut self) {
        let Some(child) = self.child.as_mut() else {
            return;
        };
        match child.try_wait() {
            Ok(Some(exit)) => {
                let code = exit.code();
                self.child = None;
                if code == Some(2) && !self.user_stopped {
                    Self::write_log(
                        &self.log,
                        "[supervisor] device disconnected (exit 2), restarting…",
                    );
                    self.status = Status::Restarting;
                    self.start();
                } else {
                    Self::write_log(&self.log, &format!("[supervisor] daemon exited (code {code:?})"));
                    self.status = Status::Stopped { code };
                }
            }
            Ok(None) => {}
            Err(e) => Self::write_log(&self.log, &format!("[supervisor] wait() error: {e}")),
        }
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(job) = self.job.take() {
            // Closing the last job handle terminates any surviving members.
            unsafe {
                let _ = CloseHandle(job);
            }
        }
    }
}
