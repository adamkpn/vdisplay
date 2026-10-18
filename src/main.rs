//! vdisplay — open any macOS app inside a virtual display of any resolution.

mod ffi;
mod launch;
mod layout;
mod objc;
mod suggest;
mod vdisplay;
mod viewer;
mod window;

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use vdisplay::{DisplayConfig, VirtualDisplay};

#[derive(Parser)]
#[command(name = "vdisplay", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a virtual display, launch an app and put its window on it.
    Run(RunArgs),
    /// Only create a virtual display and keep it until Ctrl-C.
    Create(DisplayArgs),
    /// List online displays (physical and virtual).
    List,
    /// Read a display's native resolution and suggest virtual display sizes.
    Suggest {
        /// Display ID (see `vdisplay list`). Default: main display.
        #[arg(short, long)]
        display: Option<u32>,
    },
}

#[derive(Args)]
struct DisplayArgs {
    /// Screen size the apps will see: WIDTHxHEIGHT, a preset
    /// (1080p, 1440p, 4k, 5k, 6k, 8k, ultrawide, superwide), or `native`
    /// (native pixels of the real display; see `vdisplay suggest`).
    /// Default: the size of the --view area, otherwise 4k.
    #[arg(short, long)]
    resolution: Option<ResolutionArg>,

    /// Retina: back each point with 2x2 pixels (pixel size = 2 x resolution).
    #[arg(long)]
    hidpi: bool,

    /// Refresh rate in Hz. Default: the refresh rate of the --view screen, else 60.
    #[arg(long)]
    refresh: Option<f64>,

    /// Pixel density used to report the physical size (default: 109, or 218 with --hidpi).
    #[arg(long)]
    ppi: Option<f64>,

    /// Name shown in System Settings > Displays.
    #[arg(long, default_value = "vdisplay")]
    name: String,

    /// Show the virtual display on a physical screen (scaled to fit) by
    /// mirroring it. Reverted automatically when vdisplay exits.
    #[arg(short, long)]
    mirror: bool,

    /// Physical display ID to mirror onto (see `vdisplay list`). Default: main display.
    #[arg(long, requires = "mirror")]
    mirror_to: Option<u32>,

    /// Show the virtual display live in a window covering this part of a real
    /// screen; other windows are not touched. The pointer moves onto the
    /// virtual display while it is over the window. Needs Screen Recording
    /// permission.
    #[arg(long, value_name = "AREA", conflicts_with = "mirror")]
    view: Option<layout::Area>,

    /// Physical display ID for --view (see `vdisplay list`). Default: main display.
    #[arg(long, requires = "view")]
    view_on: Option<u32>,
}

#[derive(Args)]
struct RunArgs {
    #[command(flatten)]
    display: DisplayArgs,

    /// Maximize the window instead of entering native full screen.
    #[arg(long)]
    no_fullscreen: bool,

    /// Launch a new instance even if the app is already running.
    #[arg(long)]
    new_instance: bool,

    /// Keep the virtual display after the app quits (until Ctrl-C).
    #[arg(long)]
    keep_alive: bool,

    /// Seconds to wait for the app's window to appear.
    #[arg(long, default_value_t = 30)]
    window_timeout: u64,

    /// App name ("Microsoft Edge"), .app path, or bundle id (com.microsoft.edgemac).
    app: String,

    /// Arguments passed to the app (after `--`).
    #[arg(last = true)]
    app_args: Vec<String>,
}

/// Resolution presets: names (first one is shown in help output), width, height.
const PRESETS: &[(&[&str], u32, u32)] = &[
    (&["1080p", "fhd"], 1920, 1080),
    (&["1440p", "qhd"], 2560, 1440),
    (&["4k", "uhd"], 3840, 2160),
    (&["5k"], 5120, 2880),
    (&["6k"], 6016, 3384),
    (&["8k"], 7680, 4320),
    (&["ultrawide"], 3440, 1440),
    (&["superwide"], 5120, 1440),
];

#[derive(Clone, Copy, Debug, PartialEq)]
struct Resolution {
    w: u32,
    h: u32,
}

/// `--resolution` before the physical display is known.
#[derive(Clone, Copy, Debug, PartialEq)]
enum ResolutionArg {
    Fixed(Resolution),
    Native,
}

impl FromStr for ResolutionArg {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.eq_ignore_ascii_case("native") {
            return Ok(ResolutionArg::Native);
        }
        s.parse().map(ResolutionArg::Fixed)
    }
}

impl FromStr for Resolution {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let lower = s.to_ascii_lowercase();
        if let Some(&(_, w, h)) = PRESETS.iter().find(|p| p.0.contains(&lower.as_str())) {
            return Ok(Resolution { w, h });
        }
        let (w, h) = s
            .split_once(['x', 'X', '*'])
            .ok_or_else(|| format!("expected WIDTHxHEIGHT or a preset, got '{s}'"))?;
        let w: u32 = w
            .trim()
            .parse()
            .map_err(|_| format!("bad width in '{s}'"))?;
        let h: u32 = h
            .trim()
            .parse()
            .map_err(|_| format!("bad height in '{s}'"))?;
        use suggest::{MAX_SIDE as MAX, MIN_HEIGHT, MIN_WIDTH};
        if !(MIN_WIDTH..=MAX).contains(&w) || !(MIN_HEIGHT..=MAX).contains(&h) {
            return Err(format!(
                "resolution {w}x{h} out of range ({MIN_WIDTH}x{MIN_HEIGHT} .. {MAX}x{MAX})"
            ));
        }
        Ok(Resolution { w, h })
    }
}

fn main() {
    if let Err(e) = real_main() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {
    if !cfg!(target_os = "macos") {
        bail!("vdisplay only runs on macOS");
    }
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::List => cmd_list(),
        Cmd::Suggest { display } => cmd_suggest(display),
        Cmd::Create(d) => {
            let stop = install_ctrlc()?;
            let mut session = setup_display(&d, &stop)?;
            println!(
                "Virtual display {} ready. Press Ctrl-C to remove it.",
                session.vd.id
            );
            wait_loop(&stop, &mut session, || true);
            println!("\nRemoving virtual display {}", session.vd.id);
            Ok(())
        }
        Cmd::Run(r) => cmd_run(r),
    }
}

/// A virtual display and, with --view, the window showing it. The viewer is
/// declared first so it closes before the display goes away.
struct Session {
    viewer: Option<viewer::Viewer>,
    vd: VirtualDisplay,
    /// With --view: where the viewer window is, so the app's window can be
    /// left in the same spot when the virtual display goes away.
    view_rect: Option<ffi::CGRect>,
}

fn install_ctrlc() -> Result<Arc<AtomicBool>> {
    let stop = Arc::new(AtomicBool::new(false));
    let s = stop.clone();
    ctrlc::set_handler(move || s.store(true, Ordering::SeqCst))
        .context("installing Ctrl-C handler")?;
    Ok(stop)
}

fn wait_loop(stop: &AtomicBool, session: &mut Session, keep_going: impl Fn() -> bool) {
    while !stop.load(Ordering::SeqCst) && keep_going() {
        match session.viewer.as_mut() {
            // Short steps: the pointer handoff has to keep up with the mouse,
            // and while the pointer is in the viewer it is drawn every tick.
            Some(viewer) => {
                let step = if viewer.pointer_inside() { 4 } else { 8 };
                objc::pump(Duration::from_millis(step));
                viewer.tick();
            }
            None => objc::pump(Duration::from_millis(500)),
        }
    }
}

/// Creates the display, waits for it, and mirrors it or opens the viewer
/// window if requested.
fn setup_display(d: &DisplayArgs, stop: &AtomicBool) -> Result<Session> {
    let physical_main = unsafe { ffi::CGMainDisplayID() };
    // With --view: the real display and the part of it the viewer covers.
    let view = match d.view {
        Some(area) => {
            let on = d.view_on.unwrap_or(physical_main);
            let screen = vdisplay::usable_bounds(on)
                .with_context(|| format!("display {on} not found (see `vdisplay list`)"))?;
            Some((on, area.rect(screen)))
        }
        None => None,
    };
    let mut hidpi = d.hidpi;
    let res = match (d.resolution, view) {
        (Some(ResolutionArg::Fixed(r)), _) => r,
        (Some(ResolutionArg::Native), _) => {
            let src = d.view_on.or(d.mirror_to).unwrap_or(physical_main);
            let (w, h) = native_resolution(src)?;
            println!("Using the native resolution of display {src}: {w}x{h}");
            Resolution { w, h }
        }
        // The area's own size, so the viewer shows it pixel for pixel; a
        // Retina screen gets a Retina virtual display.
        (None, Some((on, area))) => {
            hidpi |= vdisplay::backing_scale(on).unwrap_or(1.0) >= 2.0;
            Resolution {
                w: layout::even_floor(area.size.width),
                h: layout::even_floor(area.size.height),
            }
        }
        (None, None) => Resolution { w: 3840, h: 2160 },
    };
    let view_hz = view.and_then(|(on, _)| {
        vdisplay::list_displays()
            .into_iter()
            .find(|i| i.id == on)
            .map(|i| i.refresh)
    });
    let mut cfg = DisplayConfig {
        name: d.name.clone(),
        width: res.w,
        height: res.h,
        refresh: layout::refresh_rate(d.refresh, view_hz),
        hidpi,
        ppi: d.ppi.unwrap_or(if hidpi { 218.0 } else { 109.0 }),
    };
    let vd = match VirtualDisplay::create(&cfg) {
        // A rate picked from the screen may be one the virtual display
        // rejects; 60 Hz always works.
        Err(e) if d.refresh.is_none() && cfg.refresh != 60.0 => {
            eprintln!("note: {} Hz not accepted ({e:#}); using 60 Hz", cfg.refresh);
            cfg.refresh = 60.0;
            VirtualDisplay::create(&cfg)?
        }
        r => r?,
    };
    vd.wait_online(Duration::from_secs(10))?;
    let scale = if hidpi { 2 } else { 1 };
    println!(
        "Created virtual display {} — {}x{} points ({}x{} px) @ {}Hz",
        vd.id,
        cfg.width,
        cfg.height,
        cfg.width * scale,
        cfg.height * scale,
        cfg.refresh
    );

    if stop.load(Ordering::SeqCst) {
        bail!("interrupted");
    }

    if d.mirror {
        let target = d.mirror_to.unwrap_or(physical_main);
        if target == vd.id {
            bail!("cannot mirror the virtual display onto itself");
        }
        if let Some(info) = vdisplay::list_displays()
            .into_iter()
            .find(|i| i.id == target)
        {
            let phys = (info.bounds.size.width, info.bounds.size.height);
            let virt = (cfg.width as f64, cfg.height as f64);
            if !suggest::same_aspect(phys, virt) {
                eprintln!(
                    "note: aspect ratio differs from display {target} ({:.3} vs {:.3}) — expect black bars \
                     (see `vdisplay suggest`)",
                    phys.0 / phys.1,
                    virt.0 / virt.1
                );
            }
        } else {
            bail!("display {target} not found (see `vdisplay list`)");
        }
        vdisplay::mirror(target, vd.id)?;
        objc::pump(Duration::from_millis(1500));
        println!("Display {target} now mirrors the virtual display");
    } else if view.is_none() {
        println!(
            "Virtual display is an extended desktop (invisible). Use --view or --mirror to see it on your screen."
        );
    }

    let mut view_rect = None;
    let viewer = match (view, d.view) {
        (Some((on, area)), Some(name)) => {
            let rect = layout::fit(area, cfg.width as f64, cfg.height as f64);
            let pixels = (cfg.width * scale, cfg.height * scale);
            let viewer = viewer::Viewer::start(vd.id, pixels, rect, cfg.refresh)?;
            println!(
                "Showing it on the {} of display {on} ({}x{} at {},{}); move the pointer there to use it",
                name.name(),
                rect.size.width,
                rect.size.height,
                rect.origin.x,
                rect.origin.y
            );
            view_rect = Some(rect);
            Some(viewer)
        }
        _ => None,
    };
    Ok(Session {
        viewer,
        vd,
        view_rect,
    })
}

fn require_accessibility() -> Result<()> {
    if !window::is_trusted(true) {
        bail!(
            "Accessibility permission is required to move other apps' windows.\n\
             Grant it to your terminal in System Settings > Privacy & Security > Accessibility, \
             then restart the terminal and run again."
        );
    }
    Ok(())
}

fn cmd_run(r: RunArgs) -> Result<()> {
    require_accessibility()?;
    let app_path = launch::resolve_app(&r.app)?;
    let stop = install_ctrlc()?;
    let mut session = setup_display(&r.display, &stop)?;

    let app = launch::launch(&app_path, &r.app_args, r.new_instance)?;
    println!("Launched {} (pid {})", app.path, app.pid);
    app.activate();

    let win = window::wait_for_window(
        app.pid,
        Duration::from_secs(r.window_timeout),
        &stop,
        || app.is_alive(),
    )?;
    let bounds = session.vd.bounds(); // re-read: mirroring may have rearranged displays
    window::place_window(&win, bounds, !r.no_fullscreen)?;
    println!(
        "Window placed on virtual display ({}x{} at {},{}){}",
        bounds.size.width,
        bounds.size.height,
        bounds.origin.x,
        bounds.origin.y,
        if r.no_fullscreen {
            ""
        } else {
            " in full screen"
        }
    );
    println!(
        "Press Ctrl-C to remove the virtual display{}.",
        if r.keep_alive {
            ""
        } else {
            " (or quit the app)"
        }
    );

    let keep_alive = r.keep_alive;
    wait_loop(&stop, &mut session, || keep_alive || app.is_alive());
    if app.is_alive() {
        println!("\nTaking the window out of full screen");
        if let Err(e) = window::release_window(&win, session.view_rect) {
            eprintln!("warning: could not hand the window back: {e:#}");
        }
    }
    println!("Removing virtual display {}", session.vd.id);
    Ok(())
}

fn native_resolution(id: u32) -> Result<(u32, u32)> {
    let (w, h) = vdisplay::native_resolution(id).with_context(|| {
        format!("could not read the native resolution of display {id} (see `vdisplay list`)")
    })?;
    Ok((w as u32, h as u32))
}

fn cmd_suggest(display: Option<u32>) -> Result<()> {
    let main = unsafe { ffi::CGMainDisplayID() };
    let id = display.unwrap_or(main);
    let displays = vdisplay::list_displays();
    let info = displays
        .iter()
        .find(|d| d.id == id)
        .with_context(|| format!("display {id} not found (see `vdisplay list`)"))?;
    let (w, h) = native_resolution(id)?;

    let mut about = format!("{w}x{h} px native, {:.2}:1", w as f64 / h as f64);
    if let Some(mm) = vdisplay::screen_size_mm(id) {
        let inches = mm.width.hypot(mm.height) / 25.4;
        let ppi = w as f64 / (mm.width / 25.4);
        about += &format!(", {inches:.1}\" at {ppi:.0} ppi");
    }
    println!(
        "Display {id}{}: {about}",
        if id == main { " (main)" } else { "" }
    );
    println!(
        "Current mode: {}x{} points ({}x{} px) @ {:.0} Hz",
        info.bounds.size.width, info.bounds.size.height, info.pixels.0, info.pixels.1, info.refresh
    );
    if let Some(u) = vdisplay::usable_bounds(id) {
        println!(
            "Usable area (no menu bar / Dock): {}x{} at {},{} points",
            u.size.width, u.size.height, u.origin.x, u.origin.y
        );
    }
    if let Some(m) = info.mirrors {
        println!("note: this display currently mirrors display {m}");
    }

    let list = suggest::suggestions(w, h);
    println!("\nSuggested sizes (same aspect ratio, so --mirror fills the screen):");
    for s in &list {
        let flag = format!(
            "-r {}x{}{}",
            s.width,
            s.height,
            if s.hidpi { " --hidpi" } else { "" }
        );
        let (pw, ph) = s.pixels();
        let warn = if s.is_large() {
            " (above 8K, may fail)"
        } else {
            ""
        };
        println!(
            "  {flag:<24} {:<14} {}{warn}",
            format!("{pw}x{ph} px"),
            s.note
        );
    }
    let presets: Vec<&str> = PRESETS
        .iter()
        .filter(|(_, pw, ph)| suggest::same_aspect((*pw as f64, *ph as f64), (w as f64, h as f64)))
        .map(|p| p.0[0])
        .collect();
    if !presets.is_empty() {
        println!("Presets with this aspect ratio: {}", presets.join(", "));
    }

    let regions = suggest::regions(w, h);
    if !regions.is_empty() {
        println!("\nPart of the screen (1:1 with that part of the panel):");
        for r in &regions {
            let flag = format!("-r {}x{}", r.width, r.height);
            println!("  {flag:<24} {:<8} {}", r.name, r.positions);
        }
        println!(
            "  --mirror stretches to the whole screen, so these show black bars when mirrored.\n  \
             To show a virtual display in that part of the screen: vdisplay run --view left \"Microsoft Edge\""
        );
    }

    let mirror_to = if id == main {
        String::new()
    } else {
        format!(" --mirror-to {id}")
    };
    println!("\n`-r native` picks the first suggested size. Example:");
    if let Some(s) = list
        .iter()
        .find(|s| !s.hidpi && s.width > w)
        .or(list.first())
    {
        println!(
            "  vdisplay run -r {}x{} --mirror{mirror_to} \"Microsoft Edge\"",
            s.width, s.height
        );
    }
    Ok(())
}

fn cmd_list() -> Result<()> {
    let displays = vdisplay::list_displays();
    if displays.is_empty() {
        bail!("no displays found");
    }
    println!(
        "{:<12} {:<22} {:<14} {:<7} FLAGS",
        "ID", "BOUNDS (points)", "PIXELS", "HZ"
    );
    for d in displays {
        let mut flags = Vec::new();
        if d.main {
            flags.push("main".to_string());
        }
        if d.builtin {
            flags.push("builtin".to_string());
        }
        if let Some(m) = d.mirrors {
            flags.push(format!("mirrors {m}"));
        }
        println!(
            "{:<12} {:<22} {:<14} {:<7} {}",
            d.id,
            format!(
                "{}x{} @ {},{}",
                d.bounds.size.width, d.bounds.size.height, d.bounds.origin.x, d.bounds.origin.y
            ),
            format!("{}x{}", d.pixels.0, d.pixels.1),
            format!("{:.0}", d.refresh),
            flags.join(", ")
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_native_case_insensitively() {
        assert_eq!("native".parse(), Ok(ResolutionArg::Native));
        assert_eq!("NATIVE".parse(), Ok(ResolutionArg::Native));
    }

    #[test]
    fn parses_presets_aliases_and_sizes() {
        let fixed = |w, h| Ok(ResolutionArg::Fixed(Resolution { w, h }));
        assert_eq!("4k".parse(), fixed(3840, 2160));
        assert_eq!("UHD".parse(), fixed(3840, 2160));
        assert_eq!("2560x1440".parse(), fixed(2560, 1440));
        assert_eq!("2560 X 1440".parse(), fixed(2560, 1440));
    }

    #[test]
    fn rejects_bad_sizes() {
        assert!("2560".parse::<ResolutionArg>().is_err());
        assert!("100x100".parse::<ResolutionArg>().is_err());
        assert!("20000x1000".parse::<ResolutionArg>().is_err());
    }
}
