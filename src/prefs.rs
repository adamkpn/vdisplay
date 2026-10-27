//! Settings window of the menu bar app: the `run` / `create` options as form
//! controls. Every change is read back into `Settings` and saved; a value that
//! doesn't parse shows a hint under the form and nothing is saved.

use crate::ffi::*;
use crate::layout::Area;
use crate::menubar::{self, controller, with_state, Action};
use crate::objc::*;
use crate::settings::{app_title, resolution_str, Settings, Show};
use crate::{msg, suggest, vdisplay, Resolution, ResolutionArg, PRESETS};
use anyhow::{bail, Context, Result};
use clap::ValueEnum;
use std::ffi::c_void;
use std::ptr;

const W: f64 = 540.0;
const ROWS: usize = 15;
const ROW: f64 = 32.0;
const MARGIN: f64 = 20.0;
const LABEL_W: f64 = 120.0;
const H: f64 = MARGIN * 2.0 + ROWS as f64 * ROW;
const CTRL_X: f64 = MARGIN + LABEL_W + 10.0;
const CTRL_W: f64 = W - CTRL_X - MARGIN;
/// NSWindowStyleMask: titled | closable | miniaturizable.
const WINDOW_STYLE: usize = 1 | 2 | 4;
/// Popup tags of the app list beyond the apps themselves (which are 1..).
const CHOOSE_APP: isize = 10_000;
const NO_APP: isize = 10_001;
const REFRESH_RATES: [f64; 6] = [60.0, 75.0, 85.0, 100.0, 120.0, 144.0];

#[derive(Clone, Copy, Debug, PartialEq)]
enum ResChoice {
    Auto,
    Native,
    /// A listed size; true = it is meant to be rendered at 2x (turns on HiDPI).
    Fixed(Resolution, bool),
    Custom,
}

/// The window and its controls (all owned by the window, which is kept).
/// Popup items are tagged with their index in the matching list + 1.
#[derive(Clone)]
pub struct Prefs {
    window: Id,
    app: Id,
    args: Id,
    resolution: Id,
    custom: Id,
    hidpi: Id,
    refresh: Id,
    show: Id,
    screen: Id,
    fullscreen: Id,
    new_instance: Id,
    keep_alive: Id,
    name: Id,
    ppi: Id,
    timeout: Id,
    hint: Id,
    apps: Vec<String>,
    resolutions: Vec<ResChoice>,
    refreshes: Vec<Option<f64>>,
    shows: Vec<Show>,
    screens: Vec<Option<u32>>,
}

fn area_label(a: Area) -> &'static str {
    match a {
        Area::Left => "Left half",
        Area::Right => "Right half",
        Area::LeftThird => "Left third",
        Area::CenterThird => "Middle third",
        Area::RightThird => "Right third",
        Area::TopLeft => "Top-left quarter",
        Area::TopRight => "Top-right quarter",
        Area::BottomLeft => "Bottom-left quarter",
        Area::BottomRight => "Bottom-right quarter",
    }
}

/// Frame of a control in `row` (0 = top), in window coordinates (bottom-up).
fn rect(row: usize, x: f64, w: f64) -> CGRect {
    CGRect {
        origin: CGPoint {
            x,
            y: H - MARGIN - (row as f64 + 1.0) * ROW + 4.0,
        },
        size: CGSize {
            width: w,
            height: 24.0,
        },
    }
}

// ------------------------------------------------------------ control helpers

unsafe fn cls(name: &str) -> Result<Id> {
    class(name).with_context(|| format!("{name} missing"))
}

unsafe fn add_to(view: Id, control: Id, frame: CGRect) -> Id {
    msg![(); control, "setFrame:", frame => CGRect];
    msg![(); view, "addSubview:", control => Id];
    control
}

unsafe fn target_me(control: Id) {
    msg![(); control, "setTarget:", controller() => Id];
    msg![(); control, "setAction:", sel("settingChanged:") => Sel];
}

unsafe fn label(view: Id, row: usize, x: f64, w: f64, text: &str) -> Result<Id> {
    let l: Id = msg![Id; cls("NSTextField")?, "labelWithString:", ns_string(text) => Id];
    let mut r = rect(row, x, w);
    r.origin.y += 3.0;
    r.size.height = 18.0;
    Ok(add_to(view, l, r))
}

unsafe fn popup(view: Id, row: usize, x: f64, w: f64) -> Result<Id> {
    let p: Id = msg![Id; msg![Id; cls("NSPopUpButton")?, "alloc"], "initWithFrame:pullsDown:",
        rect(row, x, w) => CGRect, 0i8 => i8];
    let menu: Id = msg![Id; p, "menu"];
    msg![(); menu, "setAutoenablesItems:", 0i8 => i8];
    target_me(p);
    add_to(view, p, rect(row, x, w));
    release(p); // the view keeps it
    Ok(p)
}

unsafe fn field(view: Id, row: usize, x: f64, w: f64, placeholder: &str) -> Result<Id> {
    let f: Id = msg![Id; cls("NSTextField")?, "textFieldWithString:", ns_string("") => Id];
    msg![(); f, "setPlaceholderString:", ns_string(placeholder) => Id];
    target_me(f);
    Ok(add_to(view, f, rect(row, x, w)))
}

unsafe fn checkbox(view: Id, row: usize, title: &str) -> Result<Id> {
    let b: Id = msg![Id; cls("NSButton")?, "checkboxWithTitle:target:action:",
        ns_string(title) => Id, controller() => Id, sel("settingChanged:") => Sel];
    Ok(add_to(view, b, rect(row, CTRL_X, CTRL_W)))
}

unsafe fn button(view: Id, row: usize, x: f64, w: f64, title: &str, action: Action) -> Result<Id> {
    let b: Id = msg![Id; cls("NSButton")?, "buttonWithTitle:target:action:",
        ns_string(title) => Id, controller() => Id, sel("menuAction:") => Sel];
    msg![(); b, "setTag:", action.tag() => isize];
    Ok(add_to(view, b, rect(row, x, w)))
}

unsafe fn add_choice(popup: Id, title: &str, tag: isize) {
    let menu: Id = msg![Id; popup, "menu"];
    let item: Id = msg![Id; msg![Id; class("NSMenuItem").expect("AppKit is linked"), "alloc"],
        "initWithTitle:action:keyEquivalent:",
        ns_string(title) => Id, ptr::null::<c_void>() => Sel, ns_string("") => Id];
    msg![(); item, "setTag:", tag => isize];
    msg![(); menu, "addItem:", item => Id];
    release(item);
}

unsafe fn add_choice_separator(popup: Id) {
    let menu: Id = msg![Id; popup, "menu"];
    menubar::add_separator(menu);
}

unsafe fn clear(popup: Id) {
    msg![(); popup, "removeAllItems"];
}

unsafe fn selected_tag(popup: Id) -> isize {
    msg![isize; popup, "selectedTag"]
}

unsafe fn select_tag(popup: Id, tag: isize) {
    let _: i8 = msg![i8; popup, "selectItemWithTag:", tag => isize];
}

/// The selected entry of `list` in a popup tagged index + 1.
unsafe fn selected_in<T: Clone>(popup: Id, list: &[T]) -> Option<T> {
    let tag = selected_tag(popup);
    (tag >= 1)
        .then(|| list.get(tag as usize - 1).cloned())
        .flatten()
}

unsafe fn text(f: Id) -> String {
    nsstring_to_string(msg![Id; f, "stringValue"])
        .unwrap_or_default()
        .trim()
        .to_string()
}

unsafe fn set_text(f: Id, s: &str) {
    msg![(); f, "setStringValue:", ns_string(s) => Id];
}

unsafe fn checked(b: Id) -> bool {
    msg![isize; b, "state"] != 0
}

unsafe fn set_checked(b: Id, on: bool) {
    msg![(); b, "setState:", on as isize => isize];
}

unsafe fn set_enabled(c: Id, on: bool) {
    msg![(); c, "setEnabled:", on as i8 => i8];
}

// ------------------------------------------------------------------ window

unsafe fn build() -> Result<Prefs> {
    let frame = CGRect {
        origin: CGPoint::default(),
        size: CGSize {
            width: W,
            height: H,
        },
    };
    let window: Id = msg![Id; msg![Id; cls("NSWindow")?, "alloc"],
        "initWithContentRect:styleMask:backing:defer:",
        frame => CGRect, WINDOW_STYLE => usize, 2usize => usize, 0i8 => i8];
    if window.is_null() {
        bail!("could not create the Settings window");
    }
    msg![(); window, "setReleasedWhenClosed:", 0i8 => i8];
    msg![(); window, "setTitle:", ns_string("vdisplay Settings") => Id];
    msg![(); window, "setDelegate:", controller() => Id];
    let v: Id = msg![Id; window, "contentView"];

    let labels = [
        (0, "App"),
        (1, "App arguments"),
        (2, "Resolution"),
        (4, "Refresh rate"),
        (5, "Show"),
        (6, "Screen"),
        (7, "App window"),
        (10, "Display name"),
        (11, "Pixel density"),
        (12, "Window timeout"),
    ];
    for (row, t) in labels {
        label(v, row, MARGIN, LABEL_W, t)?;
    }
    let res_w = 250.0;
    let p = Prefs {
        window,
        app: popup(v, 0, CTRL_X, CTRL_W)?,
        args: field(
            v,
            1,
            CTRL_X,
            CTRL_W,
            "e.g. --user-data-dir=/tmp/edge-vdisplay",
        )?,
        resolution: popup(v, 2, CTRL_X, res_w)?,
        custom: field(
            v,
            2,
            CTRL_X + res_w + 8.0,
            CTRL_W - res_w - 8.0,
            "WIDTHxHEIGHT",
        )?,
        hidpi: checkbox(v, 3, "Retina (HiDPI): 2×2 pixels per point")?,
        refresh: popup(v, 4, CTRL_X, 250.0)?,
        show: popup(v, 5, CTRL_X, 250.0)?,
        screen: popup(v, 6, CTRL_X, CTRL_W)?,
        fullscreen: checkbox(v, 7, "Native full screen (otherwise maximized)")?,
        new_instance: checkbox(v, 8, "Launch a new instance even if the app is running")?,
        keep_alive: checkbox(v, 9, "Keep the display after the app quits")?,
        name: field(v, 10, CTRL_X, 250.0, "vdisplay")?,
        ppi: field(v, 11, CTRL_X, 100.0, "auto")?,
        timeout: field(v, 12, CTRL_X, 100.0, "30")?,
        hint: label(v, 13, MARGIN, W - 2.0 * MARGIN, "")?,
        apps: Vec::new(),
        resolutions: Vec::new(),
        refreshes: Vec::new(),
        shows: Vec::new(),
        screens: Vec::new(),
    };
    label(v, 11, CTRL_X + 108.0, 200.0, "ppi (empty = auto)")?;
    label(v, 12, CTRL_X + 108.0, 200.0, "seconds")?;
    let red: Id = msg![Id; cls("NSColor")?, "systemRedColor"];
    msg![(); p.hint, "setTextColor:", red => Id];
    let create = button(
        v,
        14,
        W - MARGIN - 110.0 - 8.0 - 170.0,
        170.0,
        "Create Display Only",
        Action::Create,
    )?;
    let _ = create;
    let run = button(v, 14, W - MARGIN - 110.0, 110.0, "Run", Action::Run)?;
    msg![(); run, "setKeyEquivalent:", ns_string("\r") => Id];
    msg![(); window, "center"];
    Ok(p)
}

impl Prefs {
    unsafe fn fill(&mut self, s: &Settings) {
        self.fill_apps(s);
        set_text(self.args, &s.app_args);
        self.fill_resolutions(s.screen, s.resolution);
        set_checked(self.hidpi, s.hidpi);

        clear(self.refresh);
        self.refreshes = std::iter::once(None)
            .chain(REFRESH_RATES.iter().map(|r| Some(*r)))
            .collect();
        if let Some(r) = s.refresh.filter(|r| !REFRESH_RATES.contains(r)) {
            self.refreshes.push(Some(r));
        }
        for (i, r) in self.refreshes.iter().enumerate() {
            let title = r.map_or("Auto (the screen's rate)".into(), |r| format!("{r} Hz"));
            add_choice(self.refresh, &title, i as isize + 1);
        }
        let i = self.refreshes.iter().position(|r| *r == s.refresh);
        select_tag(self.refresh, i.map_or(1, |i| i as isize + 1));

        clear(self.show);
        self.shows = Area::value_variants()
            .iter()
            .map(|a| Show::View(*a))
            .collect();
        for (i, sh) in self.shows.iter().enumerate() {
            if let Show::View(a) = sh {
                add_choice(
                    self.show,
                    &format!("In a window: {}", area_label(*a)),
                    i as isize + 1,
                );
            }
        }
        add_choice_separator(self.show);
        self.shows.push(Show::Mirror);
        add_choice(
            self.show,
            "Mirror the whole screen",
            self.shows.len() as isize,
        );
        self.shows.push(Show::Hidden);
        add_choice(
            self.show,
            "Don't show (invisible display)",
            self.shows.len() as isize,
        );
        let i = self.shows.iter().position(|x| *x == s.show).unwrap_or(0);
        select_tag(self.show, i as isize + 1);

        clear(self.screen);
        self.screens = vec![None];
        add_choice(self.screen, "Main display", 1);
        for d in vdisplay::list_displays().into_iter().filter(|d| !d.ours) {
            self.screens.push(Some(d.id));
            let mut t = format!(
                "Display {} · {}x{} pt",
                d.id, d.bounds.size.width, d.bounds.size.height
            );
            if d.main {
                t += " (main)";
            }
            if d.builtin {
                t += " (built-in)";
            }
            add_choice(self.screen, &t, self.screens.len() as isize);
        }
        if let Some(id) = s.screen.filter(|id| !self.screens.contains(&Some(*id))) {
            self.screens.push(Some(id));
            add_choice(
                self.screen,
                &format!("Display {id} (not connected: main is used)"),
                self.screens.len() as isize,
            );
        }
        let i = self
            .screens
            .iter()
            .position(|x| *x == s.screen)
            .unwrap_or(0);
        select_tag(self.screen, i as isize + 1);

        set_checked(self.fullscreen, s.fullscreen);
        set_checked(self.new_instance, s.new_instance);
        set_checked(self.keep_alive, s.keep_alive);
        set_text(self.name, &s.name);
        set_text(self.ppi, &s.ppi.map_or(String::new(), |p| p.to_string()));
        set_text(self.timeout, &s.window_timeout.to_string());
        self.hint("");
    }

    unsafe fn fill_apps(&mut self, s: &Settings) {
        clear(self.app);
        self.apps = s.recent_apps.clone();
        if self.apps.is_empty() {
            add_choice(self.app, "No app chosen", NO_APP);
        }
        for (i, a) in self.apps.iter().enumerate() {
            add_choice(self.app, &app_title(a), i as isize + 1);
        }
        add_choice_separator(self.app);
        add_choice(self.app, "Choose…", CHOOSE_APP);
        match self.apps.iter().position(|a| *a == s.app) {
            Some(i) => select_tag(self.app, i as isize + 1),
            None => select_tag(self.app, NO_APP),
        }
    }

    /// Sizes for `screen` (main if None): presets, and the sizes `vdisplay
    /// suggest` lists for that screen's panel.
    unsafe fn fill_resolutions(&mut self, screen: Option<u32>, res: Option<ResolutionArg>) {
        clear(self.resolution);
        self.resolutions.clear();
        let push = |p: &mut Prefs, c: ResChoice, title: &str| {
            p.resolutions.push(c);
            add_choice(p.resolution, title, p.resolutions.len() as isize);
        };
        push(
            self,
            ResChoice::Auto,
            "Auto (size of the view area, else 4k)",
        );
        push(self, ResChoice::Native, "Native (the screen's pixels)");
        add_choice_separator(self.resolution);
        for (names, w, h) in PRESETS {
            let r = Resolution { w: *w, h: *h };
            push(
                self,
                ResChoice::Fixed(r, false),
                &format!("{} · {w}x{h}", names[0]),
            );
        }
        let id = screen
            .filter(|id| vdisplay::active_displays().contains(id))
            .unwrap_or_else(|| CGMainDisplayID());
        if let Some((nw, nh)) = vdisplay::native_resolution(id) {
            let (nw, nh) = (nw as u32, nh as u32);
            add_choice_separator(self.resolution);
            for s in suggest::suggestions(nw, nh) {
                let r = Resolution {
                    w: s.width,
                    h: s.height,
                };
                push(
                    self,
                    ResChoice::Fixed(r, s.hidpi),
                    &format!("{}x{} · {}", s.width, s.height, s.note),
                );
            }
            for g in suggest::regions(nw, nh) {
                let r = Resolution {
                    w: g.width,
                    h: g.height,
                };
                push(
                    self,
                    ResChoice::Fixed(r, false),
                    &format!("{}x{} · a {} of the screen", g.width, g.height, g.name),
                );
            }
        }
        add_choice_separator(self.resolution);
        push(self, ResChoice::Custom, "Custom…");

        let i = self.resolutions.iter().position(|c| match (c, res) {
            (ResChoice::Auto, None) => true,
            (ResChoice::Native, Some(ResolutionArg::Native)) => true,
            (ResChoice::Fixed(r, _), Some(ResolutionArg::Fixed(x))) => *r == x,
            _ => false,
        });
        let i = i.unwrap_or(self.resolutions.len() - 1);
        select_tag(self.resolution, i as isize + 1);
        let custom = self.resolutions[i] == ResChoice::Custom;
        set_text(
            self.custom,
            &match res {
                Some(ResolutionArg::Fixed(_)) => resolution_str(res),
                _ => String::new(),
            },
        );
        set_enabled(self.custom, custom);
    }

    /// The form as settings (recent apps etc. come from `base`).
    unsafe fn read(&self, base: &Settings) -> Result<Settings, String> {
        let mut s = base.clone();
        if let Some(app) = selected_in(self.app, &self.apps) {
            s.app = app;
        }
        s.app_args = text(self.args);
        s.resolution = match selected_in(self.resolution, &self.resolutions) {
            Some(ResChoice::Auto) | None => None,
            Some(ResChoice::Native) => Some(ResolutionArg::Native),
            Some(ResChoice::Fixed(r, _)) => Some(ResolutionArg::Fixed(r)),
            Some(ResChoice::Custom) => Some(
                text(self.custom)
                    .parse::<ResolutionArg>()
                    .map_err(|e| format!("Resolution: {e}"))?,
            ),
        };
        s.hidpi = checked(self.hidpi);
        s.refresh = selected_in(self.refresh, &self.refreshes).flatten();
        s.show = selected_in(self.show, &self.shows).unwrap_or(s.show);
        s.screen = selected_in(self.screen, &self.screens).flatten();
        s.fullscreen = checked(self.fullscreen);
        s.new_instance = checked(self.new_instance);
        s.keep_alive = checked(self.keep_alive);
        s.name = text(self.name);
        if s.name.is_empty() {
            return Err("Display name can't be empty.".into());
        }
        let ppi = text(self.ppi);
        s.ppi = if ppi.is_empty() || ppi == "auto" {
            None
        } else {
            Some(
                ppi.parse::<f64>()
                    .ok()
                    .filter(|p| *p > 0.0)
                    .ok_or("Pixel density must be a positive number, or empty for auto.")?,
            )
        };
        s.window_timeout = text(self.timeout)
            .parse::<u64>()
            .ok()
            .filter(|t| *t > 0)
            .ok_or("Window timeout must be a whole number of seconds.")?;
        Ok(s)
    }

    unsafe fn hint(&self, text: &str) {
        set_text(self.hint, text);
    }

    /// Lets the user pick an .app; it becomes the current app.
    unsafe fn choose_app(&mut self) {
        let _pool = AutoreleasePool::new();
        if let (Some(panel_cls), Some(arr), Some(url)) =
            (class("NSOpenPanel"), class("NSArray"), class("NSURL"))
        {
            let panel: Id = msg![Id; panel_cls, "openPanel"];
            msg![(); panel, "setCanChooseFiles:", 1i8 => i8];
            msg![(); panel, "setCanChooseDirectories:", 0i8 => i8];
            msg![(); panel, "setAllowsMultipleSelection:", 0i8 => i8];
            let types: Id = msg![Id; arr, "arrayWithObject:", ns_string("app") => Id];
            msg![(); panel, "setAllowedFileTypes:", types => Id];
            let dir: Id = msg![Id; url, "fileURLWithPath:", ns_string("/Applications") => Id];
            msg![(); panel, "setDirectoryURL:", dir => Id];
            msg![(); panel, "setPrompt:", ns_string("Choose") => Id];
            menubar::activate();
            if msg![isize; panel, "runModal"] == 1 {
                let chosen: Id = msg![Id; panel, "URL"];
                let path = if chosen.is_null() {
                    None
                } else {
                    nsstring_to_string(msg![Id; chosen, "path"])
                };
                if let Some(path) = path {
                    with_state(|s| s.settings.remember_app(&path));
                }
            }
        }
        // Cancelled or not: show the current app again instead of "Choose…".
        if let Some(s) = with_state(|s| s.settings.clone()) {
            self.fill_apps(&s);
        }
    }
}

/// The open Settings window, if it is on screen.
fn visible() -> Option<Prefs> {
    with_state(|s| s.prefs.clone())
        .flatten()
        .filter(|p| unsafe { msg![i8; p.window, "isVisible"] } != 0)
}

/// Settings… : opens the window (built once, then reused) with the current values.
pub fn show() {
    let Some(settings) = with_state(|s| s.settings.clone()) else {
        return;
    };
    let existing = with_state(|s| s.prefs.clone()).flatten();
    let mut p = match existing {
        Some(p) => p,
        None => match unsafe { build() } {
            Ok(p) => p,
            Err(e) => {
                menubar::alert("Could not open Settings", &format!("{e:#}"));
                return;
            }
        },
    };
    unsafe {
        p.fill(&settings);
        menubar::activate();
        msg![(); p.window, "makeKeyAndOrderFront:", ptr::null_mut::<c_void>() => Id];
    }
    with_state(|s| s.prefs = Some(p));
}

/// Shows the current settings in the window if it is open (after the menu
/// changed them).
pub fn refresh() {
    let (Some(mut p), Some(settings)) = (visible(), with_state(|s| s.settings.clone())) else {
        return;
    };
    unsafe { p.fill(&settings) };
    with_state(|s| s.prefs = Some(p));
}

fn commit_with(hint: bool) -> bool {
    let (Some(p), Some(base)) = (visible(), with_state(|s| s.settings.clone())) else {
        return true;
    };
    match unsafe { p.read(&base) } {
        Ok(s) => {
            menubar::save(&s);
            with_state(|st| st.settings = s);
            unsafe { p.hint("") };
            true
        }
        Err(e) => {
            if hint {
                unsafe {
                    p.hint(&e);
                    menubar::activate();
                    msg![(); p.window, "makeKeyAndOrderFront:", ptr::null_mut::<c_void>() => Id];
                }
            }
            false
        }
    }
}

/// Saves the open window's values. False if one is invalid (the hint says
/// which): the caller must not start a session with stale settings.
pub fn commit() -> bool {
    commit_with(true)
}

/// Saves the open window's values if they are valid (closing, quitting).
pub fn commit_quietly() {
    commit_with(false);
}

/// A control changed (`settingChanged:`).
pub fn changed(sender: Id) {
    let Some(mut p) = visible() else {
        return;
    };
    unsafe {
        if sender == p.app && selected_tag(p.app) == CHOOSE_APP {
            p.choose_app();
        }
        if sender == p.resolution {
            if let Some(c) = selected_in(p.resolution, &p.resolutions) {
                if let ResChoice::Fixed(r, hidpi) = c {
                    set_text(p.custom, &format!("{}x{}", r.w, r.h));
                    if hidpi {
                        set_checked(p.hidpi, true);
                    }
                }
                set_enabled(p.custom, c == ResChoice::Custom);
            }
        }
        if sender == p.screen {
            // Offer the new screen's sizes, keeping the chosen one.
            let screen = selected_in(p.screen, &p.screens).flatten();
            let res = with_state(|s| s.settings.clone())
                .and_then(|base| p.read(&base).ok())
                .map(|s| s.resolution);
            if let Some(res) = res {
                p.fill_resolutions(screen, res);
            }
        }
    }
    with_state(|s| s.prefs = Some(p));
    commit_with(true);
}
