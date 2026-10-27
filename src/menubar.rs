//! Menu bar app (`vdisplay menu`): a status item whose menu starts and stops
//! sessions and opens the Settings window (`prefs.rs`).
//!
//! The app itself never creates a virtual display. Every session is this
//! executable running `vdisplay run …` / `create …` as a child process
//! (`sessions.rs`): `run` blocks for seconds while the app starts, the viewer
//! keeps global state (one per process), and a child that exits always takes
//! its display and mirroring with it.

use crate::ffi::*;
use crate::objc::*;
use crate::prefs::{self, Prefs};
use crate::sessions::{self, Session};
use crate::settings::{app_title, library_dir, Settings, Show};
use crate::{msg, service, vdisplay, window};
use anyhow::{bail, Context, Result};
use std::cell::RefCell;
use std::ffi::{c_void, CString};
use std::fs::File;
use std::os::fd::AsRawFd;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::time::Duration;

const ACCESSIBILITY_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";
const SCREEN_RECORDING_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture";
/// Name of the Accessibility pane; newer macOS calls it Device Control and Data Access.
const ACCESS_PANE: &str = "Accessibility (Device Control and Data Access on newer macOS)";
/// How long Quit waits for sessions to hand their windows back.
const STOP_TIMEOUT: Duration = Duration::from_secs(10);

/// NSApplicationActivationPolicyAccessory: no Dock icon, no menu bar takeover.
const ACCESSORY: isize = 1;
const NS_VARIABLE_STATUS_ITEM_LENGTH: f64 = -1.0;
const NS_IMAGE_LEFT: usize = 2;

/// Main-thread state. Never borrowed across a modal panel or alert.
pub struct State {
    pub settings: Settings,
    pub prefs: Option<Prefs>,
    sessions: Vec<Session>,
    status_item: Id,
    /// Set by SIGTERM / SIGINT (e.g. `launchctl bootout`).
    stop: &'static AtomicBool,
    /// Holds the single-instance lock while the app runs.
    _lock: File,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Runs `f` on the state; None if there is none or it is already borrowed.
pub fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok()?.as_mut().map(f))
}

/// The `VDisplayController` instance: target, menu delegate and window delegate.
static CONTROLLER: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());

pub fn controller() -> Id {
    CONTROLLER.load(Ordering::SeqCst)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Run,
    Create,
}

/// Menu items and buttons, encoded in their `tag`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    Run,
    Create,
    StopAll,
    Settings,
    Login,
    Accessibility,
    ScreenRecording,
    Logs,
    Quit,
    ResetPermissions,
    RunRecent(usize),
    Stop(i32),
    OpenLog(i32),
    Reconnect(i32),
}

const RECENT_BASE: isize = 100;
const STOP_BASE: isize = 1 << 32;
const LOG_BASE: isize = 2 << 32;
const RECONNECT_BASE: isize = 3 << 32;

impl Action {
    pub fn tag(self) -> isize {
        use Action::*;
        match self {
            Run => 1,
            Create => 2,
            StopAll => 3,
            Settings => 4,
            Login => 5,
            Accessibility => 6,
            ScreenRecording => 7,
            Logs => 8,
            Quit => 9,
            ResetPermissions => 10,
            RunRecent(i) => RECENT_BASE + i as isize,
            Stop(pid) => STOP_BASE + pid as isize,
            OpenLog(pid) => LOG_BASE + pid as isize,
            Reconnect(pid) => RECONNECT_BASE + pid as isize,
        }
    }

    fn from_tag(tag: isize) -> Option<Self> {
        use Action::*;
        Some(match tag {
            1 => Run,
            2 => Create,
            3 => StopAll,
            4 => Settings,
            5 => Login,
            6 => Accessibility,
            7 => ScreenRecording,
            8 => Logs,
            9 => Quit,
            10 => ResetPermissions,
            t if (RECENT_BASE..STOP_BASE).contains(&t) => RunRecent((t - RECENT_BASE) as usize),
            t if (STOP_BASE..LOG_BASE).contains(&t) => Stop((t - STOP_BASE) as i32),
            t if (LOG_BASE..RECONNECT_BASE).contains(&t) => OpenLog((t - LOG_BASE) as i32),
            t if (RECONNECT_BASE..4 << 32).contains(&t) => Reconnect((t - RECONNECT_BASE) as i32),
            _ => return None,
        })
    }
}

/// Takes the single-instance lock; None if another menu bar app holds it.
fn single_instance() -> Result<Option<File>> {
    let dir = library_dir("Application Support")?;
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join("menu.lock");
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    let locked = unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } == 0;
    Ok(locked.then_some(file))
}

/// Shows the status item and runs the app until Quit.
pub fn run(stop: &'static AtomicBool) -> Result<()> {
    let Some(lock) = single_instance()? else {
        // Exit 0: the LaunchAgent must not restart a duplicate.
        println!("vdisplay is already running in the menu bar");
        return Ok(());
    };
    let settings = Settings::load();
    unsafe {
        let _pool = AutoreleasePool::new();
        let app: Id =
            msg![Id; class("NSApplication").context("AppKit missing")?, "sharedApplication"];
        let _: i8 = msg![i8; app, "setActivationPolicy:", ACCESSORY => isize];

        let ctl = alloc_init(controller_class()?);
        CONTROLLER.store(ctl, Ordering::SeqCst);
        msg![(); app, "setDelegate:", ctl => Id];
        install_main_menu(app)?;

        let bar: Id =
            msg![Id; class("NSStatusBar").context("NSStatusBar missing")?, "systemStatusBar"];
        let item: Id =
            msg![Id; bar, "statusItemWithLength:", NS_VARIABLE_STATUS_ITEM_LENGTH => f64];
        if item.is_null() {
            bail!("could not create the menu bar item");
        }
        // The status bar drops items nobody retains.
        let item: Id = msg![Id; item, "retain"];
        let button: Id = msg![Id; item, "button"];
        let image = symbol("display");
        if image.is_null() {
            msg![(); button, "setTitle:", ns_string("vdisplay") => Id];
        } else {
            msg![(); image, "setTemplate:", 1i8 => i8];
            msg![(); button, "setImage:", image => Id];
            msg![(); button, "setImagePosition:", NS_IMAGE_LEFT => usize];
        }
        let menu = alloc_init(class("NSMenu").context("NSMenu missing")?);
        msg![(); menu, "setDelegate:", ctl => Id];
        msg![(); item, "setMenu:", menu => Id];
        release(menu);

        let timer_cls = class("NSTimer").context("NSTimer missing")?;
        let _: Id = msg![Id; timer_cls, "scheduledTimerWithTimeInterval:target:selector:userInfo:repeats:",
            1.0f64 => f64, ctl => Id, sel("tick:") => Sel, ptr::null_mut::<c_void>() => Id, 1i8 => i8];

        let wants_capture = matches!(settings.show, Show::View(_));
        STATE.with(|s| {
            *s.borrow_mut() = Some(State {
                settings,
                prefs: None,
                sessions: Vec::new(),
                status_item: item,
                stop,
                _lock: lock,
            })
        });

        // Ask now, so the prompts name vdisplay and come before the first Run.
        if !window::is_trusted(false) {
            window::is_trusted(true);
        }
        if wants_capture && !CGPreflightScreenCaptureAccess() {
            CGRequestScreenCaptureAccess();
        }
        msg![(); app, "run"];
    }
    Ok(())
}

/// A main menu with the standard editing commands. An accessory app shows no
/// menu bar, but text fields only get ⌘C, ⌘V, ⌘A … through these items.
unsafe fn install_main_menu(app: Id) -> Result<()> {
    let item_cls = class("NSMenuItem").context("NSMenuItem missing")?;
    let main = alloc_init(class("NSMenu").context("NSMenu missing")?);
    /// Title, action, key equivalent.
    type Command = (&'static str, &'static str, &'static str);
    let groups: [(&str, &[Command]); 2] = [
        (
            "Edit",
            &[
                ("Undo", "undo:", "z"),
                ("Redo", "redo:", "Z"),
                ("Cut", "cut:", "x"),
                ("Copy", "copy:", "c"),
                ("Paste", "paste:", "v"),
                ("Select All", "selectAll:", "a"),
            ],
        ),
        ("Window", &[("Close", "performClose:", "w")]),
    ];
    for (title, items) in groups {
        let sub = add_submenu(main, title);
        for (title, action, key) in items {
            // No target: sent to the first responder (the focused field).
            let item: Id = msg![Id; msg![Id; item_cls, "alloc"], "initWithTitle:action:keyEquivalent:",
                ns_string(title) => Id, sel(action) => Sel, ns_string(key) => Id];
            msg![(); sub, "addItem:", item => Id];
            release(item);
        }
    }
    msg![(); app, "setMainMenu:", main => Id];
    release(main);
    Ok(())
}

/// An SF Symbol image (macOS 11+), or null.
unsafe fn symbol(name: &str) -> Id {
    let Some(cls) = class("NSImage") else {
        return ptr::null_mut();
    };
    let s = sel("imageWithSystemSymbolName:accessibilityDescription:");
    if msg![i8; cls, "respondsToSelector:", s => Sel] == 0 {
        return ptr::null_mut();
    }
    msg![Id; cls, "imageWithSystemSymbolName:accessibilityDescription:",
        ns_string(name) => Id, ns_string("vdisplay") => Id]
}

/// Shows the number of running sessions next to the icon.
unsafe fn update_title(state: &State) {
    let _pool = AutoreleasePool::new();
    let button: Id = msg![Id; state.status_item, "button"];
    let has_image = !msg![Id; button, "image"].is_null();
    let n = state.sessions.len();
    let title = match (has_image, n) {
        (true, 0) => String::new(),
        (true, n) => format!(" {n}"),
        (false, 0) => "vdisplay".into(),
        (false, n) => format!("vdisplay {n}"),
    };
    msg![(); button, "setTitle:", ns_string(&title) => Id];
}

// ------------------------------------------------------------------ the menu

/// Adds an item; with an action it targets the controller, without one it is
/// a greyed-out label.
pub unsafe fn add_item(menu: Id, title: &str, action: Option<Action>, key: &str) -> Id {
    let cls = class("NSMenuItem").expect("AppKit is linked");
    let selector = if action.is_some() {
        sel("menuAction:")
    } else {
        ptr::null()
    };
    let item: Id = msg![Id; msg![Id; cls, "alloc"], "initWithTitle:action:keyEquivalent:",
        ns_string(title) => Id, selector => Sel, ns_string(key) => Id];
    if let Some(a) = action {
        msg![(); item, "setTarget:", controller() => Id];
        msg![(); item, "setTag:", a.tag() => isize];
    }
    msg![(); menu, "addItem:", item => Id];
    release(item); // the menu keeps it
    item
}

pub unsafe fn add_separator(menu: Id) {
    let sep: Id = msg![Id; class("NSMenuItem").expect("AppKit is linked"), "separatorItem"];
    msg![(); menu, "addItem:", sep => Id];
}

unsafe fn add_submenu(menu: Id, title: &str) -> Id {
    let item = add_item(menu, title, None, "");
    let sub: Id = msg![Id; msg![Id; class("NSMenu").expect("AppKit is linked"), "alloc"],
        "initWithTitle:", ns_string(title) => Id];
    msg![(); item, "setSubmenu:", sub => Id];
    release(sub);
    sub
}

unsafe fn set_checked(item: Id, on: bool) {
    msg![(); item, "setState:", on as isize => isize];
}

unsafe fn build_menu(menu: Id) {
    let _pool = AutoreleasePool::new();
    msg![(); menu, "removeAllItems"];
    with_state(|s| {
        let st = &s.settings;
        let run = if st.app.is_empty() {
            "Run… (choose an app in Settings)".to_string()
        } else {
            format!("Run {}", app_title(&st.app))
        };
        add_item(menu, &run, Some(Action::Run), "r");
        let info = add_item(menu, &st.summary(false), None, "");
        msg![(); info, "setIndentationLevel:", 1isize => isize];
        if st.recent_apps.len() > 1 {
            let sub = add_submenu(menu, "Run Other App");
            for (i, app) in st.recent_apps.iter().enumerate().skip(1) {
                add_item(sub, &app_title(app), Some(Action::RunRecent(i)), "");
            }
        }
        add_item(menu, "Create Display Only", Some(Action::Create), "");

        add_separator(menu);
        if s.sessions.is_empty() {
            add_item(menu, "No sessions running", None, "");
        } else {
            add_item(menu, "Running", None, "");
            for sess in &s.sessions {
                let title = if sess.capture_stopped {
                    format!("{} — sharing stopped", sess.label)
                } else {
                    sess.label.clone()
                };
                let sub = add_submenu(menu, &title);
                if sess.capture_stopped {
                    add_item(
                        sub,
                        "Reconnect Screen Sharing",
                        Some(Action::Reconnect(sess.pid())),
                        "",
                    );
                }
                add_item(sub, "Stop", Some(Action::Stop(sess.pid())), "");
                add_item(sub, "Open Log", Some(Action::OpenLog(sess.pid())), "");
            }
            add_item(menu, "Stop All", Some(Action::StopAll), "");
        }

        add_separator(menu);
        let displays = add_submenu(menu, "Displays");
        for d in vdisplay::list_displays() {
            let mut flags = Vec::new();
            if d.main {
                flags.push("main".to_string());
            }
            if d.builtin {
                flags.push("built-in".to_string());
            }
            if d.ours {
                flags.push("vdisplay".to_string());
            }
            if let Some(m) = d.mirrors {
                flags.push(format!("mirrors {m}"));
            }
            let title = format!(
                "{}: {}x{} pt, {}x{} px @ {:.0} Hz{}",
                d.id,
                d.bounds.size.width,
                d.bounds.size.height,
                d.pixels.0,
                d.pixels.1,
                d.refresh,
                if flags.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", flags.join(", "))
                }
            );
            add_item(displays, &title, None, "");
        }
        add_item(menu, "Settings…", Some(Action::Settings), ",");
        let login = add_item(menu, "Start at Login", Some(Action::Login), "");
        set_checked(login, service::is_installed());
        let perms = add_submenu(menu, "Permissions");
        let ax = add_item(
            perms,
            "Accessibility / Device Control (move app windows)…",
            Some(Action::Accessibility),
            "",
        );
        set_checked(ax, window::is_trusted(false));
        let sr = add_item(
            perms,
            "Screen Recording (show in a window)…",
            Some(Action::ScreenRecording),
            "",
        );
        set_checked(sr, CGPreflightScreenCaptureAccess());
        add_item(
            perms,
            "Reset Permissions…",
            Some(Action::ResetPermissions),
            "",
        );
        add_separator(perms);
        add_item(perms, "Open Logs Folder", Some(Action::Logs), "");

        add_separator(menu);
        add_item(menu, "Quit vdisplay", Some(Action::Quit), "q");
    });
}

// ------------------------------------------------------------------ actions

fn handle(action: Action) {
    match action {
        Action::Run | Action::Create => {
            if prefs::commit() {
                let kind = if action == Action::Run {
                    Kind::Run
                } else {
                    Kind::Create
                };
                start(kind);
            }
        }
        Action::RunRecent(i) => {
            prefs::commit_quietly();
            with_state(|s| {
                if let Some(app) = s.settings.recent_apps.get(i).cloned() {
                    s.settings.remember_app(&app);
                }
            });
            prefs::refresh();
            start(Kind::Run);
        }
        Action::Reconnect(pid) => reconnect(pid),
        Action::Stop(pid) => {
            with_state(|s| {
                if let Some(sess) = s.sessions.iter_mut().find(|x| x.pid() == pid) {
                    sess.stop();
                }
            });
        }
        Action::OpenLog(pid) => {
            if let Some(log) = with_state(|s| {
                s.sessions
                    .iter()
                    .find(|x| x.pid() == pid)
                    .map(|x| x.log.clone())
            })
            .flatten()
            {
                open_path(&log.to_string_lossy());
            }
        }
        Action::StopAll => {
            with_state(|s| s.sessions.iter_mut().for_each(Session::stop));
        }
        Action::Settings => prefs::show(),
        Action::Login => {
            let r = if service::is_installed() {
                service::uninstall(false)
            } else {
                service::install(false)
            };
            if let Err(e) = r {
                alert("Could not change Start at Login", &format!("{e:#}"));
            }
        }
        Action::Accessibility => {
            if !window::is_trusted(false) {
                window::is_trusted(true);
            }
            open_url(ACCESSIBILITY_URL);
        }
        Action::ResetPermissions => {
            let reset = ask(
                "Reset vdisplay's permissions?",
                "macOS forgets vdisplay's Accessibility and Screen Recording access, then \
                 asks again. Use this when vdisplay is switched on in System Settings but \
                 still says it has no access: the switch belongs to an earlier build.",
                &["Reset", "Cancel"],
            );
            if reset == 0 {
                reset_permissions();
            }
        }
        Action::ScreenRecording => {
            if unsafe { !CGPreflightScreenCaptureAccess() } {
                unsafe { CGRequestScreenCaptureAccess() };
            }
            open_url(SCREEN_RECORDING_URL);
        }
        Action::Logs => match library_dir("Logs") {
            Ok(dir) => {
                let _ = std::fs::create_dir_all(&dir);
                open_path(&dir.to_string_lossy());
            }
            Err(e) => alert("Could not open the logs folder", &format!("{e:#}")),
        },
        Action::Quit => quit(),
    }
}

/// Starts a session with the current settings.
pub fn start(kind: Kind) {
    let Some(settings) = with_state(|s| s.settings.clone()) else {
        return;
    };
    if kind == Kind::Run {
        if settings.app.is_empty() {
            // Nothing to run yet: go straight to choosing one.
            prefs::show();
            return;
        }
        if !window::is_trusted(false) {
            let text = format!(
                "vdisplay moves the app's window onto the virtual display. Allow it in \
                 System Settings > Privacy & Security > {ACCESS_PANE}, then run again.\n\n\
                 Already switched on there? Then the permission belongs to an earlier build \
                 of vdisplay (every rebuild or upgrade signed ad hoc counts as a new app). \
                 Click Reset and Ask Again, then allow it once more."
            );
            match ask(
                "vdisplay needs Accessibility permission",
                &text,
                &["Open System Settings", "Reset and Ask Again", "Cancel"],
            ) {
                0 => open_url(ACCESSIBILITY_URL),
                1 => reset_permissions(),
                _ => {}
            }
            return;
        }
    }
    // Only a prompt: a grant made now reaches the new session even though
    // this process still reads the old answer.
    if matches!(settings.show, Show::View(_)) && unsafe { !CGPreflightScreenCaptureAccess() } {
        unsafe { CGRequestScreenCaptureAccess() };
    }
    let online: Vec<u32> = vdisplay::list_displays()
        .into_iter()
        .filter(|d| !d.ours)
        .map(|d| d.id)
        .collect();
    let (args, label) = match kind {
        Kind::Run => (settings.run_args(&online), settings.summary(true)),
        Kind::Create => (
            settings.create_args(&online),
            format!("Display only · {}", settings.summary(false)),
        ),
    };
    match Session::spawn(&args, label) {
        Ok(sess) => {
            with_state(|s| {
                s.sessions.push(sess);
                if kind == Kind::Run {
                    let app = s.settings.app.clone();
                    s.settings.remember_app(&app);
                }
                save(&s.settings);
                unsafe { update_title(s) };
            });
            prefs::refresh();
        }
        Err(e) => alert("Could not start vdisplay", &format!("{e:#}")),
    }
}

pub fn save(settings: &Settings) {
    if let Err(e) = settings.save() {
        eprintln!("warning: could not save settings: {e:#}");
    }
}

fn reconnect(pid: i32) {
    with_state(|s| {
        if let Some(sess) = s.sessions.iter_mut().find(|x| x.pid() == pid) {
            sess.reconnect();
        }
    });
}

/// Every second: notice finished sessions, stopped captures and SIGTERM.
fn tick() {
    let mut failed = Vec::new();
    let mut stopped_sharing = Vec::new();
    let Some(quitting) = with_state(|s| {
        if s.stop.load(Ordering::SeqCst) {
            return true;
        }
        let before = s.sessions.len();
        s.sessions.retain_mut(|sess| match sess.finished() {
            Some(status) => {
                if let Some(why) = sess.failure(status) {
                    failed.push((sess.label.clone(), why));
                }
                false
            }
            None => true,
        });
        if s.sessions.len() != before {
            unsafe { update_title(s) };
        }
        for sess in s.sessions.iter_mut() {
            if sess.poll_capture() {
                stopped_sharing.push((sess.pid(), sess.label.clone()));
            }
        }
        false
    }) else {
        return;
    };
    if quitting {
        quit();
    }
    for (label, why) in failed {
        alert(&format!("{label} stopped"), &why);
    }
    for (pid, label) in stopped_sharing {
        let choice = ask(
            &format!("Screen sharing stopped: {label}"),
            "The window no longer shows the virtual display, and the pointer stays out of \
             it. The app is still running there.\n\nReconnect to show it again, or stop \
             the session to bring the app's window back to your screen and remove the \
             virtual display.",
            &["Reconnect", "Stop Session", "Later"],
        );
        match choice {
            0 => reconnect(pid),
            1 => handle(Action::Stop(pid)),
            _ => {}
        }
    }
}

/// Quits through AppKit, like a quit Apple Event (`brew uninstall`, logout)
/// does, so `applicationShouldTerminate:` does the cleanup in every case.
fn quit() {
    unsafe {
        if let Some(cls) = class("NSApplication") {
            let app: Id = msg![Id; cls, "sharedApplication"];
            msg![(); app, "terminate:", ptr::null_mut::<c_void>() => Id];
        }
    }
    std::process::exit(0);
}

/// Stops all sessions (they hand their windows back) before the app exits.
fn stop_everything() {
    prefs::commit_quietly();
    let mut running = with_state(|s| std::mem::take(&mut s.sessions)).unwrap_or_default();
    sessions::stop_all(&mut running, STOP_TIMEOUT);
}

/// Brings vdisplay to the front (a menu bar app is never active by itself).
pub fn activate() {
    unsafe {
        if let Some(cls) = class("NSApplication") {
            let app: Id = msg![Id; cls, "sharedApplication"];
            msg![(); app, "activateIgnoringOtherApps:", 1i8 => i8];
        }
    }
}

pub fn alert(title: &str, text: &str) {
    ask(title, text, &["OK"]);
}

/// NSAlertFirstButtonReturn; the next buttons return 1001, 1002, …
const FIRST_BUTTON: isize = 1000;

/// Shows an alert and returns the index of the button clicked (the first one
/// is the default; with no AppKit, the last one, as if cancelled).
pub fn ask(title: &str, text: &str, buttons: &[&str]) -> usize {
    let last = buttons.len().saturating_sub(1);
    unsafe {
        let _pool = AutoreleasePool::new();
        let Some(cls) = class("NSAlert") else {
            eprintln!("{title}: {text}");
            return last;
        };
        activate();
        let alert = alloc_init(cls);
        msg![(); alert, "setMessageText:", ns_string(title) => Id];
        msg![(); alert, "setInformativeText:", ns_string(text) => Id];
        for b in buttons {
            let _: Id = msg![Id; alert, "addButtonWithTitle:", ns_string(b) => Id];
        }
        let response: isize = msg![isize; alert, "runModal"];
        release(alert);
        usize::try_from(response - FIRST_BUTTON)
            .ok()
            .filter(|i| *i <= last)
            .unwrap_or(last)
    }
}

/// Makes macOS forget vdisplay's Accessibility and Screen Recording grants
/// (`tccutil reset`) and asks for Accessibility again. A grant made for an
/// earlier ad-hoc build stays switched on in System Settings but doesn't
/// match the running one, so this is the way out.
fn reset_permissions() {
    let mut failed = Vec::new();
    for service in ["Accessibility", "ScreenCapture"] {
        let out = std::process::Command::new("/usr/bin/tccutil")
            .args(["reset", service, service::BUNDLE_ID])
            .output();
        match out {
            Ok(o) if o.status.success() => {}
            Ok(o) => failed.push(format!(
                "{service}: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            )),
            Err(e) => failed.push(format!("{service}: {e}")),
        }
    }
    if !failed.is_empty() {
        alert("Could not reset the permissions", &failed.join("\n"));
        return;
    }
    // Adds vdisplay to the list again (switched off) and shows macOS's prompt.
    window::is_trusted(true);
}

fn open_url(url: &str) {
    unsafe {
        let _pool = AutoreleasePool::new();
        let (Some(ws), Some(url_cls)) = (class("NSWorkspace"), class("NSURL")) else {
            return;
        };
        let ws: Id = msg![Id; ws, "sharedWorkspace"];
        let url: Id = msg![Id; url_cls, "URLWithString:", ns_string(url) => Id];
        if !url.is_null() {
            let _: i8 = msg![i8; ws, "openURL:", url => Id];
        }
    }
}

fn open_path(path: &str) {
    unsafe {
        let _pool = AutoreleasePool::new();
        let (Some(ws), Some(url_cls)) = (class("NSWorkspace"), class("NSURL")) else {
            return;
        };
        let ws: Id = msg![Id; ws, "sharedWorkspace"];
        let url: Id = msg![Id; url_cls, "fileURLWithPath:", ns_string(path) => Id];
        let _: i8 = msg![i8; ws, "openURL:", url => Id];
    }
}

// ------------------------------------------------------------ the controller

unsafe extern "C" fn menu_needs_update(_this: Id, _cmd: Sel, menu: Id) {
    build_menu(menu);
}

unsafe extern "C" fn menu_action(_this: Id, _cmd: Sel, sender: Id) {
    let tag: isize = msg![isize; sender, "tag"];
    if let Some(a) = Action::from_tag(tag) {
        handle(a);
    }
}

unsafe extern "C" fn timer_tick(_this: Id, _cmd: Sel, _timer: Id) {
    let _pool = AutoreleasePool::new();
    tick();
}

unsafe extern "C" fn setting_changed(_this: Id, _cmd: Sel, sender: Id) {
    prefs::changed(sender);
}

unsafe extern "C" fn window_will_close(_this: Id, _cmd: Sel, _note: Id) {
    prefs::commit_quietly();
}

/// NSTerminateNow
const TERMINATE_NOW: usize = 1;

unsafe extern "C" fn should_terminate(_this: Id, _cmd: Sel, _app: Id) -> usize {
    stop_everything();
    TERMINATE_NOW
}

/// Runtime class `VDisplayController`.
unsafe fn controller_class() -> Result<Id> {
    const NAME: &str = "VDisplayController";
    if let Some(cls) = class(NAME) {
        return Ok(cls);
    }
    let name = CString::new(NAME)?;
    let cls = objc_allocateClassPair(
        class("NSObject").context("NSObject missing")?,
        name.as_ptr(),
        0,
    );
    if cls.is_null() {
        bail!("could not create the {NAME} class");
    }
    let types = CString::new("v@:@")?;
    let methods: [(&str, unsafe extern "C" fn(Id, Sel, Id)); 5] = [
        ("menuNeedsUpdate:", menu_needs_update),
        ("menuAction:", menu_action),
        ("tick:", timer_tick),
        ("settingChanged:", setting_changed),
        ("windowWillClose:", window_will_close),
    ];
    for (selector, imp) in methods {
        class_addMethod(cls, sel(selector), imp as *const c_void, types.as_ptr());
    }
    let types = CString::new("Q@:@")?;
    class_addMethod(
        cls,
        sel("applicationShouldTerminate:"),
        should_terminate as *const c_void,
        types.as_ptr(),
    );
    for proto in [
        "NSMenuDelegate",
        "NSWindowDelegate",
        "NSApplicationDelegate",
    ] {
        let proto = CString::new(proto)?;
        let proto = objc_getProtocol(proto.as_ptr());
        if !proto.is_null() {
            class_addProtocol(cls, proto);
        }
    }
    objc_registerClassPair(cls);
    Ok(cls)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_round_trip() {
        use Action::*;
        for a in [
            Run,
            Create,
            StopAll,
            Settings,
            Login,
            Accessibility,
            ScreenRecording,
            Logs,
            Quit,
            ResetPermissions,
            RunRecent(0),
            RunRecent(7),
            Stop(1),
            Stop(i32::MAX),
            OpenLog(4242),
            Reconnect(77),
        ] {
            assert_eq!(Action::from_tag(a.tag()), Some(a));
        }
        assert_eq!(Action::from_tag(0), None);
        assert_eq!(Action::from_tag(50), None);
    }
}
