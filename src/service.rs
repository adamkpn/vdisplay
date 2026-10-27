//! Background service: a LaunchAgent that starts the menu bar app at login and
//! restarts it if it crashes (but not after Quit, which exits with 0).

use crate::ffi::getuid;
use crate::settings::{library_dir, write_file};
use anyhow::{bail, Context, Result};
use std::path::{Component, Path, PathBuf};
use std::process::Command;

/// CFBundleIdentifier in packaging/Info.plist.
pub const BUNDLE_ID: &str = "io.github.adamkpn.vdisplay";
/// The LaunchAgent's label: the bundle id.
pub const LABEL: &str = BUNDLE_ID;

pub fn plist_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home)
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist")))
}

/// Whether `exe` is `<something>.app/Contents/MacOS/<exe>`.
pub fn in_app_bundle(exe: &Path) -> bool {
    fn named<'a>(p: Option<&'a Path>, name: &str) -> Option<&'a Path> {
        p.filter(|p| p.file_name().is_some_and(|n| n == name))
    }
    let contents = named(
        named(exe.parent(), "MacOS").and_then(Path::parent),
        "Contents",
    );
    contents
        .and_then(Path::parent)
        .and_then(Path::extension)
        .is_some_and(|e| e == "app")
}

/// A Homebrew keg path, `<prefix>/Cellar/<name>/<version>/…`, as the
/// `<prefix>/opt/<name>/…` link, which still exists after `brew upgrade`
/// removes that version. Other paths are returned unchanged.
pub fn stable_path(exe: &Path) -> PathBuf {
    let parts: Vec<Component> = exe.components().collect();
    match parts.iter().position(|c| c.as_os_str() == "Cellar") {
        Some(i) if parts.len() > i + 3 => {
            let mut p: PathBuf = parts[..i].iter().collect();
            p.push("opt");
            p.push(parts[i + 1]);
            p.extend(&parts[i + 3..]);
            p
        }
        _ => exe.to_path_buf(),
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The LaunchAgent property list for running `exe menu`, logging to `log`.
pub fn plist(exe: &str, log: &str) -> String {
    let (exe, log) = (xml_escape(exe), xml_escape(log));
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{exe}</string>
		<string>menu</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<dict>
		<key>SuccessfulExit</key>
		<false/>
	</dict>
	<key>LimitLoadToSessionType</key>
	<string>Aqua</string>
	<key>ProcessType</key>
	<string>Interactive</string>
	<key>StandardOutPath</key>
	<string>{log}</string>
	<key>StandardErrorPath</key>
	<string>{log}</string>
</dict>
</plist>
"#
    )
}

fn target() -> String {
    format!("gui/{}/{LABEL}", unsafe { getuid() })
}

fn launchctl(args: &[&str]) -> Result<std::process::Output> {
    Command::new("/bin/launchctl")
        .args(args)
        .output()
        .context("running launchctl")
}

/// Whether launchd has the agent loaded in this login session.
pub fn is_loaded() -> bool {
    launchctl(&["print", &target()]).is_ok_and(|o| o.status.success())
}

pub fn is_installed() -> bool {
    plist_path().is_ok_and(|p| p.exists())
}

/// Writes the LaunchAgent for this executable. With `start`, also (re)loads
/// it now, which starts the menu bar app; otherwise it starts at next login.
pub fn install(start: bool) -> Result<()> {
    let exe = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .context("locating the vdisplay executable")?;
    if !in_app_bundle(&exe) {
        eprintln!(
            "note: {} is not inside vdisplay.app, so permissions are granted to this path. \
             Install the app (Homebrew, or `make install-app`) and run it from there instead.",
            exe.display()
        );
    }
    let exe = Some(stable_path(&exe))
        .filter(|p| p.exists())
        .unwrap_or(exe);
    let log = library_dir("Logs")?.join("menu.log");
    std::fs::create_dir_all(log.parent().context("log path has no directory")?)?;
    let path = plist_path()?;
    write_file(
        &path,
        &plist(&exe.to_string_lossy(), &log.to_string_lossy()),
    )?;
    if start {
        if is_loaded() {
            // Picks up a new path; also stops a running instance.
            let _ = launchctl(&["bootout", &target()]);
        }
        let domain = format!("gui/{}", unsafe { getuid() });
        let out = launchctl(&["bootstrap", &domain, &path.to_string_lossy()])?;
        if !out.status.success() {
            bail!(
                "launchctl bootstrap failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
    }
    Ok(())
}

/// Removes the LaunchAgent. With `stop`, also unloads it, which quits a menu
/// bar app it started; otherwise that one keeps running until logout.
pub fn uninstall(stop: bool) -> Result<()> {
    if stop && is_loaded() {
        let out = launchctl(&["bootout", &target()])?;
        if !out.status.success() {
            bail!(
                "launchctl bootout failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
    }
    let path = plist_path()?;
    if path.exists() {
        std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    }
    Ok(())
}

pub fn cmd_install() -> Result<()> {
    install(true)?;
    println!(
        "Installed {} and started the menu bar app; it starts again at every login.",
        plist_path()?.display()
    );
    Ok(())
}

pub fn cmd_uninstall() -> Result<()> {
    uninstall(true)?;
    println!("Removed the LaunchAgent {LABEL}; vdisplay no longer starts at login.");
    Ok(())
}

pub fn cmd_status() -> Result<()> {
    let path = plist_path()?;
    if path.exists() {
        println!("Installed: {}", path.display());
    } else {
        println!("Not installed (vdisplay service install)");
    }
    let out = launchctl(&["print", &target()])?;
    if !out.status.success() {
        println!("Not loaded in this login session");
        return Ok(());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines().map(str::trim) {
        if ["state = ", "pid = ", "program = ", "last exit code = "]
            .iter()
            .any(|k| line.starts_with(k))
        {
            println!("  {line}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_app_bundles() {
        assert!(in_app_bundle(Path::new(
            "/Applications/vdisplay.app/Contents/MacOS/vdisplay"
        )));
        assert!(!in_app_bundle(Path::new("/opt/homebrew/bin/vdisplay")));
        assert!(!in_app_bundle(Path::new("/x/vdisplay.app/MacOS/vdisplay")));
        assert!(!in_app_bundle(Path::new("/x/Contents/MacOS/vdisplay")));
    }

    #[test]
    fn homebrew_kegs_use_the_opt_link() {
        assert_eq!(
            stable_path(Path::new(
                "/opt/homebrew/Cellar/vdisplay/0.2.0/vdisplay.app/Contents/MacOS/vdisplay"
            )),
            Path::new("/opt/homebrew/opt/vdisplay/vdisplay.app/Contents/MacOS/vdisplay")
        );
        assert_eq!(
            stable_path(Path::new("/usr/local/Cellar/vdisplay/0.2.0_1/bin/vdisplay")),
            Path::new("/usr/local/opt/vdisplay/bin/vdisplay")
        );
        let app = Path::new("/Applications/vdisplay.app/Contents/MacOS/vdisplay");
        assert_eq!(stable_path(app), app);
        let short = Path::new("/opt/homebrew/Cellar/vdisplay");
        assert_eq!(stable_path(short), short);
    }

    #[test]
    fn plist_runs_the_menu_and_escapes_paths() {
        let p = plist("/Apps/A&B.app/Contents/MacOS/vdisplay", "/tmp/menu.log");
        assert!(p.contains("<string>/Apps/A&amp;B.app/Contents/MacOS/vdisplay</string>"));
        assert!(p.contains("<string>menu</string>"));
        assert!(p.contains("<key>SuccessfulExit</key>\n\t\t<false/>"));
        assert!(p.contains(&format!("<string>{LABEL}</string>")));
        assert_eq!(p.matches("/tmp/menu.log").count(), 2);
    }
}
