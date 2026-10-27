//! What the menu bar app starts: the `run` / `create` options, stored as
//! `key=value` lines, and the `vdisplay` arguments they turn into. Pure logic
//! apart from `load` / `save`.

use crate::layout::Area;
use crate::{Resolution, ResolutionArg};
use anyhow::{Context, Result};
use clap::ValueEnum;
use std::path::{Path, PathBuf};

/// Apps kept in the "Run" submenu and the Settings app list.
const MAX_RECENT: usize = 8;

/// How the virtual display is shown.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Show {
    /// `--view AREA`: a live window on part of a real screen.
    View(Area),
    /// `--mirror`: the whole real screen mirrors it.
    Mirror,
    /// Neither: an invisible extended desktop.
    Hidden,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// App name, .app path or bundle id; empty when none is chosen.
    pub app: String,
    /// Arguments for the app, shell-like (see `split_args`).
    pub app_args: String,
    /// Most recent first; includes `app`.
    pub recent_apps: Vec<String>,
    /// None: the CLI default (size of the view area, else 4k).
    pub resolution: Option<ResolutionArg>,
    pub hidpi: bool,
    pub refresh: Option<f64>,
    pub show: Show,
    /// Real display to view on / mirror to; None = main display.
    pub screen: Option<u32>,
    pub fullscreen: bool,
    pub new_instance: bool,
    pub keep_alive: bool,
    pub name: String,
    pub ppi: Option<f64>,
    pub window_timeout: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            // No app until the user chooses one in Settings.
            app: String::new(),
            app_args: String::new(),
            recent_apps: Vec::new(),
            resolution: None,
            hidpi: false,
            refresh: None,
            show: Show::View(Area::Left),
            screen: None,
            fullscreen: true,
            new_instance: false,
            keep_alive: false,
            name: "vdisplay".into(),
            ppi: None,
            window_timeout: 30,
        }
    }
}

pub fn resolution_str(r: Option<ResolutionArg>) -> String {
    match r {
        None => "auto".into(),
        Some(ResolutionArg::Native) => "native".into(),
        Some(ResolutionArg::Fixed(Resolution { w, h })) => format!("{w}x{h}"),
    }
}

pub fn show_str(s: Show) -> String {
    match s {
        Show::View(a) => a.name(),
        Show::Mirror => "mirror".into(),
        Show::Hidden => "hidden".into(),
    }
}

fn parse_show(s: &str) -> Option<Show> {
    match s {
        "mirror" => Some(Show::Mirror),
        "hidden" => Some(Show::Hidden),
        _ => Area::from_str(s, true).ok().map(Show::View),
    }
}

/// `auto` / empty = None, else a positive number.
fn parse_opt_f64(s: &str) -> Option<Option<f64>> {
    if s.is_empty() || s == "auto" {
        return Some(None);
    }
    s.parse::<f64>().ok().filter(|v| *v > 0.0).map(Some)
}

fn opt_f64_str(v: Option<f64>) -> String {
    v.map_or("auto".into(), |v| v.to_string())
}

/// What a path or bundle id is called in the menu: "/Applications/Safari.app"
/// becomes "Safari".
pub fn app_title(app: &str) -> String {
    if app.contains('/') || app.ends_with(".app") {
        let name = app.trim_end_matches('/').rsplit('/').next().unwrap_or(app);
        return name.trim_end_matches(".app").to_string();
    }
    app.to_string()
}

impl Settings {
    /// Reads settings written by `serialize`. Unknown keys and bad values are
    /// skipped, so an old or hand-edited file still loads.
    pub fn parse(text: &str) -> Self {
        let mut s = Settings::default();
        let mut recent = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let (key, value) = (key.trim(), value.trim());
            let flag = || match value {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            };
            match key {
                "app" => s.app = value.to_string(),
                "args" => s.app_args = value.to_string(),
                "recent" if !value.is_empty() => recent.push(value.to_string()),
                "resolution" if value == "auto" => s.resolution = None,
                "resolution" => {
                    if let Ok(r) = value.parse() {
                        s.resolution = Some(r);
                    }
                }
                "hidpi" => s.hidpi = flag().unwrap_or(s.hidpi),
                "refresh" => s.refresh = parse_opt_f64(value).unwrap_or(s.refresh),
                "show" => s.show = parse_show(value).unwrap_or(s.show),
                "screen" if value == "main" => s.screen = None,
                "screen" => s.screen = value.parse().ok().or(s.screen),
                "fullscreen" => s.fullscreen = flag().unwrap_or(s.fullscreen),
                "new_instance" => s.new_instance = flag().unwrap_or(s.new_instance),
                "keep_alive" => s.keep_alive = flag().unwrap_or(s.keep_alive),
                "name" if !value.is_empty() => s.name = value.to_string(),
                "ppi" => s.ppi = parse_opt_f64(value).unwrap_or(s.ppi),
                "window_timeout" => {
                    s.window_timeout = value
                        .parse()
                        .ok()
                        .filter(|t| *t > 0)
                        .unwrap_or(s.window_timeout)
                }
                _ => {}
            }
        }
        if !recent.is_empty() || !s.app.is_empty() {
            s.recent_apps = recent;
            if !s.app.is_empty() {
                let app = s.app.clone();
                s.remember_app(&app);
            }
        }
        s
    }

    pub fn serialize(&self) -> String {
        let mut out = String::from("# vdisplay menu bar settings\n");
        // One line per key: a line break in a value (a pasted argument list)
        // would otherwise start a new key.
        let mut kv = |k: &str, v: &str| {
            let v = v.replace(['\r', '\n'], " ");
            out.push_str(&format!("{k}={v}\n"))
        };
        kv("app", &self.app);
        kv("args", &self.app_args);
        for r in &self.recent_apps {
            kv("recent", r);
        }
        kv("resolution", &resolution_str(self.resolution));
        kv("hidpi", &self.hidpi.to_string());
        kv("refresh", &opt_f64_str(self.refresh));
        kv("show", &show_str(self.show));
        kv(
            "screen",
            &self.screen.map_or("main".into(), |id| id.to_string()),
        );
        kv("fullscreen", &self.fullscreen.to_string());
        kv("new_instance", &self.new_instance.to_string());
        kv("keep_alive", &self.keep_alive.to_string());
        kv("name", &self.name);
        kv("ppi", &opt_f64_str(self.ppi));
        kv("window_timeout", &self.window_timeout.to_string());
        out
    }

    /// Makes `app` the current app and the first recent one.
    pub fn remember_app(&mut self, app: &str) {
        self.app = app.to_string();
        self.recent_apps.retain(|a| a != app);
        self.recent_apps.insert(0, app.to_string());
        self.recent_apps.truncate(MAX_RECENT);
    }

    /// Flags shared by `run` and `create`. `online` are the displays now
    /// online: a remembered screen that is gone falls back to the main one.
    fn display_args(&self, online: &[u32]) -> Vec<String> {
        let mut a = Vec::new();
        if self.resolution.is_some() {
            a.extend(["-r".into(), resolution_str(self.resolution)]);
        }
        if self.hidpi {
            a.push("--hidpi".into());
        }
        if let Some(hz) = self.refresh {
            a.extend(["--refresh".into(), hz.to_string()]);
        }
        if let Some(ppi) = self.ppi {
            a.extend(["--ppi".into(), ppi.to_string()]);
        }
        a.extend(["--name".into(), self.name.clone()]);
        let screen = self.screen.filter(|id| online.contains(id));
        match self.show {
            Show::View(area) => {
                a.extend(["--view".into(), area.name()]);
                if let Some(id) = screen {
                    a.extend(["--view-on".into(), id.to_string()]);
                }
            }
            Show::Mirror => {
                a.push("--mirror".into());
                if let Some(id) = screen {
                    a.extend(["--mirror-to".into(), id.to_string()]);
                }
            }
            Show::Hidden => {}
        }
        a
    }

    /// Arguments for `vdisplay run …` (without the program name).
    pub fn run_args(&self, online: &[u32]) -> Vec<String> {
        let mut a = vec!["run".to_string()];
        a.extend(self.display_args(online));
        if !self.fullscreen {
            a.push("--no-fullscreen".into());
        }
        if self.new_instance {
            a.push("--new-instance".into());
        }
        if self.keep_alive {
            a.push("--keep-alive".into());
        }
        a.extend([
            "--window-timeout".into(),
            self.window_timeout.to_string(),
            self.app.clone(),
        ]);
        let app_args = split_args(&self.app_args);
        if !app_args.is_empty() {
            a.push("--".into());
            a.extend(app_args);
        }
        a
    }

    /// Arguments for `vdisplay create …` (without the program name).
    pub fn create_args(&self, online: &[u32]) -> Vec<String> {
        let mut a = vec!["create".to_string()];
        a.extend(self.display_args(online));
        a
    }

    /// One line for the menu: "Microsoft Edge · 5k · left".
    pub fn summary(&self, with_app: bool) -> String {
        let mut parts = Vec::new();
        if with_app {
            parts.push(app_title(&self.app));
        }
        parts.push(match self.resolution {
            None => "auto size".into(),
            r => resolution_str(r),
        });
        parts.push(match self.show {
            Show::View(a) => a.name(),
            Show::Mirror => "mirrored".into(),
            Show::Hidden => "hidden".into(),
        });
        parts.join(" · ")
    }

    pub fn load() -> Self {
        settings_path()
            .ok()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|t| Settings::parse(&t))
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        let path = settings_path()?;
        write_file(&path, &self.serialize())
    }
}

/// Splits app arguments like a shell would, without expansion: whitespace
/// separates, single and double quotes group.
pub fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    for c in s.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                in_word = true;
            }
            (None, c) if c.is_whitespace() => {
                if in_word {
                    out.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            (None, c) => {
                cur.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        out.push(cur);
    }
    out
}

/// `~/Library/<sub>/vdisplay`.
pub fn library_dir(sub: &str) -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join(sub)
        .join("vdisplay"))
}

fn settings_path() -> Result<PathBuf> {
    Ok(library_dir("Application Support")?.join("settings.conf"))
}

/// Writes `text` to `path`, creating its directory.
pub fn write_file(path: &Path, text: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Cli;
    use clap::Parser;

    fn parses(args: Vec<String>) {
        let full: Vec<String> = std::iter::once("vdisplay".to_string())
            .chain(args)
            .collect();
        if let Err(e) = Cli::try_parse_from(&full) {
            panic!("{full:?} is not a valid command line: {e}");
        }
    }

    #[test]
    fn round_trips_through_the_file_format() {
        let mut s = Settings {
            app_args: "--user-data-dir=/tmp/edge \"a b\"".into(),
            resolution: Some(ResolutionArg::Fixed(Resolution { w: 5120, h: 2880 })),
            hidpi: true,
            refresh: Some(120.0),
            show: Show::View(Area::BottomRight),
            screen: Some(3),
            fullscreen: false,
            new_instance: true,
            keep_alive: true,
            name: "edge 5k".into(),
            ppi: Some(140.5),
            window_timeout: 45,
            ..Settings::default()
        };
        s.remember_app("/Applications/Safari.app");
        assert_eq!(Settings::parse(&s.serialize()), s);
        let native = Settings {
            resolution: Some(ResolutionArg::Native),
            show: Show::Mirror,
            ..Settings::default()
        };
        assert_eq!(Settings::parse(&native.serialize()), native);
    }

    #[test]
    fn line_breaks_in_values_cannot_add_keys() {
        let s = Settings {
            app: "Safari".into(),
            app_args: "--a\napp=/tmp/Evil.app\r\nhidpi=true".into(),
            ..Settings::default()
        };
        let back = Settings::parse(&s.serialize());
        assert_eq!(back.app, "Safari");
        assert!(!back.hidpi);
        assert_eq!(back.app_args, "--a app=/tmp/Evil.app  hidpi=true");
    }

    #[test]
    fn starts_without_an_app() {
        let s = Settings::default();
        assert!(s.app.is_empty() && s.recent_apps.is_empty());
        assert_eq!(Settings::parse(""), s);
    }

    #[test]
    fn skips_bad_lines_and_keeps_defaults() {
        let s = Settings::parse("junk\nshow=sideways\nrefresh=-1\nhidpi=yes\nwindow_timeout=0\n");
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn generated_arguments_are_valid_cli() {
        let shows = [
            Show::View(Area::Left),
            Show::View(Area::CenterThird),
            Show::Mirror,
            Show::Hidden,
        ];
        let resolutions = [
            None,
            Some(ResolutionArg::Native),
            Some(ResolutionArg::Fixed(Resolution { w: 2560, h: 1440 })),
        ];
        for show in shows {
            for resolution in resolutions {
                for screen in [None, Some(7)] {
                    let s = Settings {
                        show,
                        resolution,
                        screen,
                        hidpi: screen.is_some(),
                        refresh: screen.map(|_| 85.0),
                        ppi: screen.map(|_| 218.0),
                        fullscreen: screen.is_none(),
                        new_instance: screen.is_some(),
                        keep_alive: screen.is_some(),
                        app: "Microsoft Edge".into(),
                        app_args: "--incognito 'x y'".into(),
                        ..Settings::default()
                    };
                    parses(s.run_args(&[1, 7]));
                    parses(s.create_args(&[1, 7]));
                }
            }
        }
    }

    #[test]
    fn maps_options_to_flags() {
        let s = Settings {
            app: "Microsoft Edge".into(),
            screen: Some(7),
            app_args: "--incognito".into(),
            ..Settings::default()
        };
        assert_eq!(
            s.run_args(&[1, 7]),
            [
                "run",
                "--name",
                "vdisplay",
                "--view",
                "left",
                "--view-on",
                "7",
                "--window-timeout",
                "30",
                "Microsoft Edge",
                "--",
                "--incognito"
            ]
        );
        // Screen 7 is gone: fall back to the main display.
        assert!(!s.run_args(&[1]).contains(&"--view-on".to_string()));
        let m = Settings {
            show: Show::Mirror,
            screen: Some(7),
            ..Settings::default()
        };
        let a = m.create_args(&[7]);
        assert!(a.windows(2).any(|w| w == ["--mirror-to", "7"]));
    }

    #[test]
    fn splits_arguments_like_a_shell() {
        assert_eq!(
            split_args(r#"  --a=1 "b c" 'd "e"' f""g  "#),
            ["--a=1", "b c", "d \"e\"", "fg"]
        );
        assert_eq!(split_args(r#"x """#), ["x", ""]);
        assert!(split_args("   ").is_empty());
    }

    #[test]
    fn remembers_recent_apps() {
        let mut s = Settings::default();
        for i in 0..10 {
            s.remember_app(&format!("App {i}"));
        }
        s.remember_app("App 5");
        assert_eq!(s.app, "App 5");
        assert_eq!(s.recent_apps.len(), MAX_RECENT);
        assert_eq!(s.recent_apps[0], "App 5");
        assert_eq!(s.recent_apps.iter().filter(|a| *a == "App 5").count(), 1);
    }

    #[test]
    fn titles_apps() {
        assert_eq!(app_title("/Applications/Safari.app"), "Safari");
        assert_eq!(app_title("/Applications/Safari.app/"), "Safari");
        assert_eq!(app_title("Microsoft Edge"), "Microsoft Edge");
        assert_eq!(app_title("com.microsoft.edgemac"), "com.microsoft.edgemac");
    }
}
