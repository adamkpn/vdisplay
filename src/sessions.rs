//! Sessions started from the menu bar: each one is this executable running
//! `vdisplay run …` or `vdisplay create …` as a child process, with its output
//! in a log file. Stopping one sends SIGTERM, which takes the same clean exit
//! path as Ctrl-C in a terminal.

use crate::ffi::kill;
use crate::settings::library_dir;
use crate::viewer::{STATUS_RUNNING, STATUS_STOPPED};
use anyhow::{Context, Result};
use std::fs::File;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SIGKILL: i32 = 9;
const SIGTERM: i32 = 15;
const SIGUSR1: i32 = 30;
/// Session logs kept in ~/Library/Logs/vdisplay; older ones are deleted.
const KEEP_LOGS: usize = 20;

pub struct Session {
    child: Child,
    /// Shown in the menu.
    pub label: String,
    pub log: PathBuf,
    /// Set by `stop`: the exit that follows is not reported as a failure.
    pub stopping: bool,
    /// The session's viewer reported that its screen capture stopped.
    pub capture_stopped: bool,
    /// Log size when the status was last read.
    log_len: u64,
}

impl Session {
    /// Starts `vdisplay <args>`.
    pub fn spawn(args: &[String], label: String) -> Result<Self> {
        let exe = std::env::current_exe().context("locating the vdisplay executable")?;
        let dir = library_dir("Logs")?;
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        prune_logs(&dir);
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let log = dir.join(format!(
            "session-{}-{:03}.log",
            stamp.as_secs(),
            stamp.subsec_millis()
        ));
        let out = File::create(&log).with_context(|| format!("creating {}", log.display()))?;
        let err = out.try_clone()?;
        let child = Command::new(&exe)
            .args(args)
            // The session ends by itself if this app goes away.
            .env(crate::PARENT_ENV, std::process::id().to_string())
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err)
            .spawn()
            .with_context(|| format!("starting {}", exe.display()))?;
        Ok(Session {
            child,
            label,
            log,
            stopping: false,
            capture_stopped: false,
            log_len: 0,
        })
    }

    pub fn pid(&self) -> i32 {
        self.child.id() as i32
    }

    /// Asks the session to remove its display and exit.
    pub fn stop(&mut self) {
        self.stopping = true;
        unsafe { kill(self.pid(), SIGTERM) };
    }

    /// Asks the session's viewer to start capturing again (it ignores this
    /// unless its capture stopped; only sent then, since SIGUSR1 would end a
    /// session without a viewer).
    pub fn reconnect(&mut self) {
        if self.capture_stopped {
            unsafe { kill(self.pid(), SIGUSR1) };
        }
    }

    /// Re-reads the capture status from the log if it grew. Returns true
    /// when it changed to stopped.
    pub fn poll_capture(&mut self) -> bool {
        let len = std::fs::metadata(&self.log).map_or(0, |m| m.len());
        if len == self.log_len {
            return false;
        }
        self.log_len = len;
        let log = std::fs::read_to_string(&self.log).unwrap_or_default();
        let Some(stopped) = capture_status(&log) else {
            return false;
        };
        let newly = stopped && !self.capture_stopped;
        self.capture_stopped = stopped;
        newly
    }

    /// The exit status once the child has exited.
    pub fn finished(&mut self) -> Option<ExitStatus> {
        self.child.try_wait().ok().flatten()
    }

    /// Why a session that exited on its own failed: the `error:` the CLI
    /// printed, else the exit status.
    pub fn failure(&self, status: ExitStatus) -> Option<String> {
        if self.stopping || status.success() {
            return None;
        }
        let log = std::fs::read_to_string(&self.log).unwrap_or_default();
        Some(last_error(&log).unwrap_or_else(|| format!("vdisplay exited ({status}).")))
    }
}

/// Stops every session and waits up to `timeout` for them to clean up; the
/// ones still running then are killed.
pub fn stop_all(sessions: &mut Vec<Session>, timeout: Duration) {
    for s in sessions.iter_mut() {
        s.stop();
    }
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        sessions.retain_mut(|s| s.finished().is_none());
        if sessions.is_empty() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    for s in sessions.iter_mut() {
        unsafe { kill(s.pid(), SIGKILL) };
        let _ = s.child.wait();
    }
    sessions.clear();
}

/// The last `error: …` the CLI printed (it can span several lines), without
/// the prefix.
pub fn last_error(log: &str) -> Option<String> {
    let lines: Vec<&str> = log.lines().collect();
    let first = lines.iter().rposition(|l| l.starts_with("error: "))?;
    let msg = lines[first..].join("\n");
    let msg = msg["error: ".len()..].trim();
    (!msg.is_empty()).then(|| msg.to_string())
}

/// Whether the last capture status a viewer printed is "stopped"; None if it
/// printed none.
pub fn capture_status(log: &str) -> Option<bool> {
    log.lines().rev().find_map(|l| match l.trim() {
        STATUS_STOPPED => Some(true),
        STATUS_RUNNING => Some(false),
        _ => None,
    })
}

/// Deletes all but the newest `KEEP_LOGS` session logs.
fn prune_logs(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut logs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("session-") && n.ends_with(".log"))
        })
        .collect();
    // The names sort by start time.
    logs.sort();
    let excess = logs.len().saturating_sub(KEEP_LOGS - 1);
    for p in &logs[..excess] {
        let _ = std::fs::remove_file(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_last_error_with_its_continuation_lines() {
        let log = "Created virtual display 5\nerror: first\nerror: Accessibility permission is required.\n\
                   Grant it in System Settings.\n";
        assert_eq!(
            last_error(log).as_deref(),
            Some("Accessibility permission is required.\nGrant it in System Settings.")
        );
    }

    #[test]
    fn reads_the_latest_capture_status() {
        assert_eq!(capture_status("Created virtual display 5\n"), None);
        assert_eq!(
            capture_status(&format!("x\n{STATUS_STOPPED}\nScreen capture stopped…\n")),
            Some(true)
        );
        assert_eq!(
            capture_status(&format!("{STATUS_STOPPED}\n{STATUS_RUNNING}\n")),
            Some(false)
        );
    }

    #[test]
    fn ignores_logs_without_an_error_line() {
        assert_eq!(last_error("note: 85 Hz not accepted (error: x)\n"), None);
        assert_eq!(last_error(""), None);
        assert_eq!(last_error("error: \n"), None);
    }
}
