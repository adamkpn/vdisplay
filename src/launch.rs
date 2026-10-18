//! Launching applications through NSWorkspace (gives us the PID back).

use crate::ffi::*;
use crate::msg;
use crate::objc::*;
use anyhow::{bail, Context, Result};
use std::path::Path;
use std::ptr;

/// NSWorkspaceLaunchNewInstance
const LAUNCH_NEW_INSTANCE: usize = 0x0008_0000;
/// NSApplicationActivateIgnoringOtherApps
const ACTIVATE_IGNORING_OTHER_APPS: usize = 1 << 1;

pub struct LaunchedApp {
    pub pid: i32,
    pub path: String,
    running: Id,
}

impl LaunchedApp {
    pub fn is_alive(&self) -> bool {
        unsafe { kill(self.pid, 0) == 0 }
    }

    pub fn activate(&self) {
        unsafe {
            let _: i8 = msg![i8; self.running, "activateWithOptions:", ACTIVATE_IGNORING_OTHER_APPS => usize];
        }
    }
}

impl Drop for LaunchedApp {
    fn drop(&mut self) {
        unsafe { release(self.running) }
    }
}

/// Resolves an app given as a name ("Microsoft Edge"), a bundle path
/// ("/Applications/Safari.app") or a bundle id ("com.microsoft.edgemac").
pub fn resolve_app(app: &str) -> Result<String> {
    if app.contains('/') || app.ends_with(".app") {
        let p = Path::new(app);
        if !p.exists() {
            bail!("application bundle not found: {app}");
        }
        return Ok(p.canonicalize()?.to_string_lossy().into_owned());
    }
    unsafe {
        let _pool = AutoreleasePool::new();
        let ws: Id = msg![Id; class("NSWorkspace").context("AppKit missing")?, "sharedWorkspace"];
        let s = CfStr::new(app);
        // Bundle identifier?
        if app.matches('.').count() >= 2 && !app.contains(' ') {
            let url: Id = msg![Id; ws, "URLForApplicationWithBundleIdentifier:", s.as_id() => Id];
            if !url.is_null() {
                if let Some(p) = nsstring_to_string(msg![Id; url, "path"]) {
                    return Ok(p);
                }
            }
        }
        // Display name (deprecated API, but still the simplest name lookup).
        let path: Id = msg![Id; ws, "fullPathForApplication:", s.as_id() => Id];
        nsstring_to_string(path).with_context(|| {
            format!("could not find an application named '{app}' (try the full .app path)")
        })
    }
}

pub fn launch(app_path: &str, args: &[String], new_instance: bool) -> Result<LaunchedApp> {
    unsafe {
        let _pool = AutoreleasePool::new();
        let ws: Id = msg![Id; class("NSWorkspace").context("AppKit missing")?, "sharedWorkspace"];
        let path = CfStr::new(app_path);
        let url: Id = msg![Id; class("NSURL").context("NSURL missing")?, "fileURLWithPath:", path.as_id() => Id];

        let arr: Id = msg![Id; class("NSMutableArray").context("NSMutableArray missing")?, "array"];
        for a in args {
            let s = CfStr::new(a);
            msg![(); arr, "addObject:", s.as_id() => Id];
        }
        let config: Id = msg![Id; class("NSDictionary").context("NSDictionary missing")?,
            "dictionaryWithObject:forKey:", arr => Id, NSWorkspaceLaunchConfigurationArguments => Id];

        let options = if new_instance { LAUNCH_NEW_INSTANCE } else { 0 };
        let mut err: Id = ptr::null_mut();
        let running: Id = msg![Id; ws, "launchApplicationAtURL:options:configuration:error:",
            url => Id, options => usize, config => Id, &mut err as *mut Id => *mut Id];
        if running.is_null() {
            let reason = if err.is_null() {
                "unknown error".to_string()
            } else {
                nsstring_to_string(msg![Id; err, "localizedDescription"]).unwrap_or_default()
            };
            bail!("failed to launch {app_path}: {reason}");
        }
        let running: Id = msg![Id; running, "retain"];
        let pid: i32 = msg![i32; running, "processIdentifier"];
        Ok(LaunchedApp {
            pid,
            path: app_path.to_string(),
            running,
        })
    }
}
