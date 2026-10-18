//! Live view of a virtual display in a borderless window on a real screen.
//!
//! Frames come from ScreenCaptureKit (CGDisplayStream is gone since macOS 15).
//! Input works the way DeskPad does it: while the pointer is over the window
//! it is moved onto the virtual display, so clicks, scrolling, hover and
//! typing reach the app there directly; at the window's edge it is moved back.

use crate::ffi::*;
use crate::layout;
use crate::msg;
use crate::objc::*;
use anyhow::{bail, Context, Result};
use std::ffi::{c_void, CString};
use std::mem::size_of;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Layer the frames are drawn into (only ever touched on the frame queue and
/// in `Drop`); null when no viewer is open.
static LAYER: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
/// Last IOSurface shown, to force a redraw when ScreenCaptureKit reuses one.
static LAST_SURFACE: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static FRAMES: AtomicU64 = AtomicU64::new(0);

/// `VDISPLAY_STATS=1`: report frame rate, latency and loop gaps every 2 s.
static STATS: AtomicBool = AtomicBool::new(false);
/// Capture-to-viewer latency of the frames in the current stats window, in µs.
static LATENCY_SUM_US: AtomicU64 = AtomicU64::new(0);
static LATENCY_MAX_US: AtomicU64 = AtomicU64::new(0);
const STATS_EVERY: Duration = Duration::from_secs(2);

/// Main-loop side of the stats (only touched on the main thread).
struct Stats {
    /// False until the first tick: startup (placing the app's window) runs
    /// without ticks and would show up as one long gap.
    running: bool,
    since: Instant,
    frames_at_start: u64,
    last_tick: Instant,
    max_gap: Duration,
    /// Longest time vdisplay itself spent in one tick (pointer handoff and
    /// drawing); the rest of a gap is the event loop or the system.
    max_work: Duration,
}

impl Stats {
    fn new() -> Self {
        let now = Instant::now();
        Stats {
            running: false,
            since: now,
            frames_at_start: FRAMES.load(Ordering::SeqCst),
            last_tick: now,
            max_gap: Duration::ZERO,
            max_work: Duration::ZERO,
        }
    }

    /// Records a tick; prints and resets once per `STATS_EVERY`.
    fn tick(&mut self) {
        if !self.running {
            LATENCY_SUM_US.store(0, Ordering::SeqCst);
            LATENCY_MAX_US.store(0, Ordering::SeqCst);
            *self = Stats {
                running: true,
                ..Stats::new()
            };
            return;
        }
        let now = Instant::now();
        self.max_gap = self.max_gap.max(now - self.last_tick);
        self.last_tick = now;
        let elapsed = now - self.since;
        if elapsed < STATS_EVERY {
            return;
        }
        let frames = FRAMES.load(Ordering::SeqCst) - self.frames_at_start;
        let sum = LATENCY_SUM_US.swap(0, Ordering::SeqCst);
        let max = LATENCY_MAX_US.swap(0, Ordering::SeqCst);
        let avg_ms = if frames == 0 {
            0.0
        } else {
            sum as f64 / frames as f64 / 1000.0
        };
        eprintln!(
            "stats: {:5.1} fps captured | capture->viewer latency avg {:4.1} ms, max {:4.1} ms | main loop max gap {:4.1} ms (tick work max {:4.1} ms)",
            frames as f64 / elapsed.as_secs_f64(),
            avg_ms,
            max as f64 / 1000.0,
            self.max_gap.as_secs_f64() * 1000.0,
            self.max_work.as_secs_f64() * 1000.0
        );
        *self = Stats {
            running: true,
            ..Stats::new()
        };
    }
}

/// Where the real pointer is, and its last known position.
#[derive(Clone, Copy, Debug)]
enum Pointer {
    /// On a real screen.
    Outside(CGPoint),
    /// Moved onto the virtual display because it entered the viewer.
    Inside(CGPoint),
}

/// NSActivityUserInitiated | NSActivityLatencyCritical: no App Nap and no timer
/// coalescing for a background process, so frames and the pointer stay on time.
const ACTIVITY_OPTIONS: u64 = 0x00FF_FFFF | 0xFF_0000_0000;
/// Without `CGSCurrentCursorSeed`, how often the pointer's shape is re-read.
const POINTER_SHAPE_FALLBACK_EVERY: Duration = Duration::from_millis(250);

/// `CGSCurrentCursorSeed` (private, CoreGraphics): a counter that changes
/// whenever the pointer's shape does, read from shared memory. Asking for the
/// shape itself (`currentSystemCursor`) is a window-server round trip that
/// takes 3-40 ms about one call in seven, so it is only done on a change.
fn cursor_seed() -> Option<i32> {
    static SEED: OnceLock<Option<extern "C" fn() -> i32>> = OnceLock::new();
    let seed = SEED.get_or_init(|| unsafe {
        let name = CString::new("CGSCurrentCursorSeed").ok()?;
        let f = dlsym(RTLD_DEFAULT, name.as_ptr());
        (!f.is_null()).then(|| std::mem::transmute::<*mut c_void, extern "C" fn() -> i32>(f))
    });
    seed.map(|f| f())
}

/// The pointer drawn over the frames while the real one is on the virtual
/// display. Drawn here, it follows the hand at the loop's rate instead of
/// arriving with each captured frame.
struct PointerLayer {
    layer: Id,
    shown: bool,
    frame: CGRect,
    hot_spot: CGPoint,
    size: CGSize,
    /// `cursor_seed()` when the shape was last read.
    shape_seed: Option<i32>,
    shape_read: Option<Instant>,
}

pub struct Viewer {
    window: Id,
    stream: Id,
    output: Id,
    /// Serial user-interactive queue frames are delivered and drawn on, so a
    /// busy main loop can't hold them back.
    frame_queue: Id,
    /// Sublayer showing the frames (+1 retained).
    frames_layer: Id,
    /// Token from `beginActivityWithOptions:reason:`.
    activity: Id,
    pointer_layer: PointerLayer,
    /// The window's content, in global CoreGraphics coordinates.
    rect: CGRect,
    display: CGDirectDisplayID,
    pointer: Pointer,
    stats: Option<Stats>,
}

impl Viewer {
    /// Shows virtual display `display` (`pixels` = its backing size) in a
    /// window covering `rect`, capturing up to `fps` frames per second.
    pub fn start(
        display: CGDirectDisplayID,
        pixels: (u32, u32),
        rect: CGRect,
        fps: f64,
    ) -> Result<Self> {
        unsafe {
            if !CGPreflightScreenCaptureAccess() {
                CGRequestScreenCaptureAccess();
                bail!(
                    "Screen Recording permission is required to show the virtual display in a window.\n\
                     Grant it to your terminal in System Settings > Privacy & Security > \
                     Screen & System Audio Recording, then restart the terminal and run again."
                );
            }
            let path = CString::new(
                "/System/Library/Frameworks/ScreenCaptureKit.framework/ScreenCaptureKit",
            )?;
            if dlopen(path.as_ptr(), RTLD_NOW).is_null() {
                bail!("ScreenCaptureKit is not available (needs macOS 12.3 or newer)");
            }
            let _pool = AutoreleasePool::new();
            let app: Id =
                msg![Id; class("NSApplication").context("AppKit missing")?, "sharedApplication"];
            // Accessory: a window, but no Dock icon and no menu bar takeover.
            let _: i8 = msg![i8; app, "setActivationPolicy:", 1isize => isize];
            msg![(); app, "finishLaunching"];
            set_event_app(app);

            let info: Id =
                msg![Id; class("NSProcessInfo").context("NSProcessInfo missing")?, "processInfo"];
            let reason = CfStr::new("vdisplay live view");
            let activity: Id = msg![Id; info, "beginActivityWithOptions:reason:",
                ACTIVITY_OPTIONS => u64, reason.as_id() => Id];
            let activity = if activity.is_null() {
                activity
            } else {
                msg![Id; activity, "retain"]
            };

            let label = CString::new("vdisplay.frames")?;
            let attr =
                dispatch_queue_attr_make_with_qos_class(ptr::null(), QOS_CLASS_USER_INTERACTIVE, 0);
            let frame_queue = dispatch_queue_create(label.as_ptr(), attr);
            let (window, frames_layer, pointer_layer) = create_window(rect)?;
            let mut viewer = Viewer {
                window,
                stream: ptr::null_mut(),
                output: ptr::null_mut(),
                frame_queue,
                frames_layer,
                activity,
                pointer_layer: PointerLayer {
                    layer: pointer_layer,
                    shown: false,
                    frame: CGRect::default(),
                    hot_spot: CGPoint::default(),
                    size: CGSize::default(),
                    shape_seed: None,
                    shape_read: None,
                },
                rect,
                display,
                pointer: Pointer::Outside(mouse_location()),
                stats: None,
            };
            let sc_display = find_sc_display(display)?;
            let started = start_stream(sc_display, pixels, fps, frame_queue);
            release(sc_display);
            (viewer.stream, viewer.output) = started?;

            let deadline = Instant::now() + Duration::from_secs(5);
            while FRAMES.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
                pump(Duration::from_millis(50));
            }
            if FRAMES.load(Ordering::SeqCst) == 0 {
                eprintln!(
                    "warning: no frames from the virtual display yet. If the window stays black, \
                     check Screen Recording permission for your terminal."
                );
            }
            if std::env::var("VDISPLAY_STATS").is_ok_and(|v| !v.is_empty() && v != "0") {
                STATS.store(true, Ordering::SeqCst);
                viewer.stats = Some(Stats::new());
                eprintln!("stats: on (fps counts changed frames only; an idle page reads ~0)");
            }
            Ok(viewer)
        }
    }

    /// Moves the pointer between the viewer and the virtual display. Call it
    /// every few milliseconds.
    pub fn tick(&mut self) {
        if let Some(stats) = self.stats.as_mut() {
            stats.tick();
        }
        let work = Instant::now();
        let p = mouse_location();
        let virt = unsafe { CGDisplayBounds(self.display) };
        self.pointer = match self.pointer {
            // Reached the virtual display across a shared edge, not through
            // the viewer: put it back.
            Pointer::Outside(last) if virt.contains(p) => {
                warp(last);
                Pointer::Outside(last)
            }
            Pointer::Outside(_) if self.rect.contains(p) => {
                let v = layout::map_point(self.rect, virt, p);
                warp(v);
                Pointer::Inside(v)
            }
            Pointer::Outside(_) => Pointer::Outside(p),
            // Pinned at an edge with a real screen beyond the viewer: leave.
            Pointer::Inside(_) if virt.contains(p) => {
                match self.exit(layout::exit_point(self.rect, virt, p, true)) {
                    Some(out) => Pointer::Outside(out),
                    None => Pointer::Inside(p),
                }
            }
            // Crossed into another display: continue from the viewer's edge,
            // or go back if there is nowhere to go on that side.
            Pointer::Inside(last) => {
                match self.exit(layout::exit_point(self.rect, virt, last, false)) {
                    Some(out) => Pointer::Outside(out),
                    None => {
                        warp(last);
                        Pointer::Inside(last)
                    }
                }
            }
        };
        match self.pointer {
            Pointer::Inside(v) => self.draw_pointer(layout::map_point(virt, self.rect, v)),
            Pointer::Outside(_) => self.hide_pointer(),
        }
        if let Some(stats) = self.stats.as_mut() {
            stats.max_work = stats.max_work.max(work.elapsed());
        }
    }

    /// Whether the real pointer is on the virtual display (over the viewer).
    pub fn pointer_inside(&self) -> bool {
        matches!(self.pointer, Pointer::Inside(_))
    }

    /// Shows the pointer at `at` (global coordinates inside the viewer).
    fn draw_pointer(&mut self, at: CGPoint) {
        let _pool = AutoreleasePool::new();
        let Some(tx) = class("CATransaction") else {
            return;
        };
        let pl = &mut self.pointer_layer;
        unsafe {
            msg![(); tx, "begin"];
            msg![(); tx, "setDisableActions:", 1i8 => i8];
            // The app under the pointer sets its shape (arrow, I-beam, hand).
            let seed = cursor_seed();
            let stale = match seed {
                Some(_) => pl.shape_seed != seed,
                None => pl
                    .shape_read
                    .is_none_or(|t| t.elapsed() >= POINTER_SHAPE_FALLBACK_EVERY),
            };
            if stale {
                pl.shape_seed = seed;
                pl.shape_read = Some(Instant::now());
                let cursor: Id = match class("NSCursor") {
                    Some(c) => msg![Id; c, "currentSystemCursor"],
                    None => ptr::null_mut(),
                };
                let image: Id = if cursor.is_null() {
                    ptr::null_mut()
                } else {
                    msg![Id; cursor, "image"]
                };
                if !image.is_null() {
                    let cg: Id = msg![Id; image, "CGImageForProposedRect:context:hints:",
                        ptr::null_mut::<CGRect>() => *mut CGRect,
                        ptr::null_mut::<c_void>() => Id,
                        ptr::null_mut::<c_void>() => Id];
                    if !cg.is_null() {
                        msg![(); pl.layer, "setContents:", cg => Id];
                        pl.hot_spot = msg![CGPoint; cursor, "hotSpot"];
                        pl.size = msg![CGSize; image, "size"];
                    }
                }
            }
            // Layer coordinates run bottom-up; the hot spot is measured from
            // the image's top-left corner.
            let (x, y) = (at.x - self.rect.origin.x, at.y - self.rect.origin.y);
            let frame = CGRect {
                origin: CGPoint {
                    x: x - pl.hot_spot.x,
                    y: self.rect.size.height - y - (pl.size.height - pl.hot_spot.y),
                },
                size: pl.size,
            };
            if frame != pl.frame {
                msg![(); pl.layer, "setFrame:", frame => CGRect];
                pl.frame = frame;
            }
            if !pl.shown {
                msg![(); pl.layer, "setHidden:", 0i8 => i8];
                pl.shown = true;
            }
            msg![(); tx, "commit"];
        }
    }

    fn hide_pointer(&mut self) {
        let pl = &mut self.pointer_layer;
        if !pl.shown {
            return;
        }
        if let Some(tx) = class("CATransaction") {
            unsafe {
                msg![(); tx, "begin"];
                msg![(); tx, "setDisableActions:", 1i8 => i8];
                msg![(); pl.layer, "setHidden:", 1i8 => i8];
                msg![(); tx, "commit"];
            }
        }
        pl.shown = false;
        // Re-read the shape on the next entry.
        pl.shape_seed = None;
        pl.shape_read = None;
    }

    /// Warps to `target` if it is on a real screen outside the viewer.
    fn exit(&self, target: Option<CGPoint>) -> Option<CGPoint> {
        let t = target?;
        let on_screen = crate::vdisplay::active_displays()
            .into_iter()
            .any(|id| id != self.display && unsafe { CGDisplayBounds(id) }.contains(t));
        (on_screen && !self.rect.contains(t)).then(|| {
            warp(t);
            t
        })
    }
}

impl Drop for Viewer {
    fn drop(&mut self) {
        unsafe {
            // Hand the pointer back where the user sees it.
            if let Pointer::Inside(p) = self.pointer {
                warp(layout::map_point(
                    CGDisplayBounds(self.display),
                    self.rect,
                    p,
                ));
            }
            LAYER.store(ptr::null_mut(), Ordering::SeqCst);
            if !self.stream.is_null() {
                msg![(); self.stream, "stopCaptureWithCompletionHandler:", ptr::null_mut::<c_void>() => Id];
                pump(Duration::from_millis(200)); // let the stop settle
            }
            // Wait out a frame that is being drawn right now: the layers
            // below must outlive it.
            if !self.frame_queue.is_null() {
                dispatch_sync_f(self.frame_queue, ptr::null_mut(), drained);
            }
            release(self.stream);
            release(self.output);
            release(self.frames_layer);
            release(self.pointer_layer.layer);
            if !self.frame_queue.is_null() {
                dispatch_release(self.frame_queue);
            }
            if !self.activity.is_null() {
                if let Some(info) = class("NSProcessInfo") {
                    let info: Id = msg![Id; info, "processInfo"];
                    msg![(); info, "endActivity:", self.activity => Id];
                }
                release(self.activity);
            }
            if !self.window.is_null() {
                msg![(); self.window, "orderOut:", ptr::null_mut::<c_void>() => Id];
                release(self.window);
            }
        }
    }
}

fn mouse_location() -> CGPoint {
    unsafe {
        let event = CGEventCreate(ptr::null());
        if event.is_null() {
            return CGPoint::default();
        }
        let p = CGEventGetLocation(event);
        CFRelease(event);
        p
    }
}

fn warp(p: CGPoint) {
    unsafe {
        CGWarpMouseCursorPosition(p);
        // Re-associating ends the short freeze macOS applies after a warp.
        CGAssociateMouseAndMouseCursorPosition(1);
    }
}

/// Borderless, shadowless window over `rect` with two sublayers (both +1
/// retained): one showing the frames scaled to fit, and a hidden one above it
/// for the pointer. Shown without activating vdisplay.
unsafe fn create_window(rect: CGRect) -> Result<(Id, Id, Id)> {
    let main_h = CGDisplayBounds(CGMainDisplayID()).size.height;
    let frame = CGRect {
        origin: CGPoint {
            x: rect.origin.x,
            y: main_h - rect.origin.y - rect.size.height,
        },
        size: rect.size,
    };
    let window: Id = msg![Id; msg![Id; class("NSWindow").context("NSWindow missing")?, "alloc"],
        "initWithContentRect:styleMask:backing:defer:",
        frame => CGRect, 0usize => usize, 2usize => usize, 0i8 => i8];
    if window.is_null() {
        bail!("could not create the viewer window");
    }
    msg![(); window, "setReleasedWhenClosed:", 0i8 => i8];
    msg![(); window, "setOpaque:", 1i8 => i8];
    msg![(); window, "setHasShadow:", 0i8 => i8];
    let black: Id = msg![Id; class("NSColor").context("NSColor missing")?, "blackColor"];
    msg![(); window, "setBackgroundColor:", black => Id];
    let view: Id = msg![Id; window, "contentView"];
    msg![(); view, "setWantsLayer:", 1i8 => i8];
    let layer: Id = msg![Id; view, "layer"];
    let scale: f64 = msg![f64; window, "backingScaleFactor"];
    let layer_cls = class("CALayer").context("CALayer missing")?;

    // An own layer, not the view's: AppKit expects its layers to be changed on
    // the main thread only, and frames are drawn on the frame queue.
    let frames = alloc_init(layer_cls);
    let bounds = CGRect {
        origin: CGPoint::default(),
        size: rect.size,
    };
    msg![(); frames, "setFrame:", bounds => CGRect];
    const WIDTH_AND_HEIGHT_SIZABLE: u32 = 2 | 16; // kCALayerWidthSizable | kCALayerHeightSizable
    msg![(); frames, "setAutoresizingMask:", WIDTH_AND_HEIGHT_SIZABLE => u32];
    let gravity = CfStr::new("resizeAspect");
    msg![(); frames, "setContentsGravity:", gravity.as_id() => Id];
    msg![(); frames, "setContentsScale:", scale => f64];
    msg![(); layer, "addSublayer:", frames => Id];
    LAYER.store(frames, Ordering::SeqCst);

    let pointer = alloc_init(layer_cls);
    msg![(); pointer, "setHidden:", 1i8 => i8];
    msg![(); pointer, "setContentsScale:", scale => f64];
    msg![(); layer, "addSublayer:", pointer => Id];

    msg![(); window, "orderFrontRegardless"];
    Ok((window, frames, pointer))
}

/// No-op run on the frame queue by `Drop` to wait for in-flight frames.
extern "C" fn drained(_: *mut c_void) {}

// ------------------------------------------------------------ ScreenCaptureKit

/// Finds the `SCDisplay` for `display` (+1 retained). A new virtual display
/// can take a moment to show up in ScreenCaptureKit, so this retries.
unsafe fn find_sc_display(display: CGDirectDisplayID) -> Result<Id> {
    for _ in 0..10 {
        let content = shareable_content()?;
        let displays: Id = msg![Id; content, "displays"];
        let count = if displays.is_null() {
            0
        } else {
            msg![usize; displays, "count"]
        };
        for i in 0..count {
            let d: Id = msg![Id; displays, "objectAtIndex:", i => usize];
            if msg![u32; d, "displayID"] == display {
                let d: Id = msg![Id; d, "retain"];
                release(content);
                return Ok(d);
            }
        }
        release(content);
        pump(Duration::from_millis(500));
    }
    bail!("ScreenCaptureKit does not list virtual display {display}")
}

static CONTENT: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static CONTENT_ERR: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static CONTENT_DONE: AtomicBool = AtomicBool::new(false);

/// Completion handler of `getShareableContentWithCompletionHandler:` (runs on
/// a ScreenCaptureKit queue).
unsafe extern "C" fn on_content(_block: *mut Block, content: Id, err: Id) {
    if !content.is_null() {
        CONTENT.store(msg![Id; content, "retain"], Ordering::SeqCst);
    }
    if !err.is_null() {
        CONTENT_ERR.store(msg![Id; err, "retain"], Ordering::SeqCst);
    }
    CONTENT_DONE.store(true, Ordering::SeqCst);
}

/// `[SCShareableContent getShareableContentWithCompletionHandler:]`, waited
/// for while the run loop keeps pumping. Returns +1 retained content.
unsafe fn shareable_content() -> Result<Id> {
    CONTENT_DONE.store(false, Ordering::SeqCst);
    let cls = class("SCShareableContent").context("SCShareableContent missing")?;
    msg![(); cls, "getShareableContentWithCompletionHandler:", global_block(on_content as *const c_void) => Id];
    let deadline = Instant::now() + Duration::from_secs(10);
    while !CONTENT_DONE.load(Ordering::SeqCst) {
        if Instant::now() >= deadline {
            bail!("timed out waiting for ScreenCaptureKit");
        }
        pump(Duration::from_millis(20));
    }
    let err = CONTENT_ERR.swap(ptr::null_mut(), Ordering::SeqCst);
    if !err.is_null() {
        let reason = nsstring_to_string(msg![Id; err, "localizedDescription"]).unwrap_or_default();
        release(err);
        bail!("ScreenCaptureKit: {reason} (check Screen Recording permission for your terminal)");
    }
    let content = CONTENT.swap(ptr::null_mut(), Ordering::SeqCst);
    if content.is_null() {
        bail!("ScreenCaptureKit returned no shareable content");
    }
    Ok(content)
}

/// Starts capturing `sc_display` at `pixels` and up to `fps`, frames delivered
/// on `queue`. Returns the stream and its output object (both +1 retained).
unsafe fn start_stream(
    sc_display: Id,
    pixels: (u32, u32),
    fps: f64,
    queue: Id,
) -> Result<(Id, Id)> {
    let empty: Id = msg![Id; class("NSArray").context("NSArray missing")?, "array"];
    let filter: Id = msg![Id; msg![Id; class("SCContentFilter").context("SCContentFilter missing")?, "alloc"],
        "initWithDisplay:excludingWindows:", sc_display => Id, empty => Id];
    let config =
        alloc_init(class("SCStreamConfiguration").context("SCStreamConfiguration missing")?);
    msg![(); config, "setWidth:", pixels.0 as usize => usize];
    msg![(); config, "setHeight:", pixels.1 as usize => usize];
    msg![(); config, "setPixelFormat:", u32::from_be_bytes(*b"BGRA") => u32];
    // The viewer draws the pointer itself (see `Viewer::draw_pointer`): one
    // baked into the frames would trail the hand by the whole capture delay.
    msg![(); config, "setShowsCursor:", 0i8 => i8];
    msg![(); config, "setQueueDepth:", 5isize => isize];
    let interval = CMTime {
        value: 1,
        timescale: fps.round().max(1.0) as i32,
        flags: kCMTimeFlags_Valid,
        epoch: 0,
    };
    msg![(); config, "setMinimumFrameInterval:", interval => CMTime];

    let stream: Id = msg![Id; msg![Id; class("SCStream").context("SCStream missing")?, "alloc"],
        "initWithFilter:configuration:delegate:", filter => Id, config => Id, ptr::null_mut::<c_void>() => Id];
    release(filter);
    release(config);
    if stream.is_null() {
        bail!("could not create the ScreenCaptureKit stream");
    }
    let output = alloc_init(output_class()?);
    let mut err: Id = ptr::null_mut();
    let ok: i8 = msg![i8; stream, "addStreamOutput:type:sampleHandlerQueue:error:",
        output => Id, 0isize => isize, queue => Id, &mut err as *mut Id => *mut Id];
    if ok == 0 {
        let reason = if err.is_null() {
            "unknown error".to_string()
        } else {
            nsstring_to_string(msg![Id; err, "localizedDescription"]).unwrap_or_default()
        };
        release(stream);
        release(output);
        bail!("could not attach to the ScreenCaptureKit stream: {reason}");
    }
    msg![(); stream, "startCaptureWithCompletionHandler:", ptr::null_mut::<c_void>() => Id];
    Ok((stream, output))
}

/// Runtime class implementing `SCStreamOutput`.
unsafe fn output_class() -> Result<Id> {
    const NAME: &str = "VDisplayStreamOutput";
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
    let types = CString::new("v@:@^{opaqueCMSampleBuffer=}q")?;
    class_addMethod(
        cls,
        sel("stream:didOutputSampleBuffer:ofType:"),
        did_output as *const c_void,
        types.as_ptr(),
    );
    let proto = CString::new("SCStreamOutput")?;
    let proto = objc_getProtocol(proto.as_ptr());
    if !proto.is_null() {
        class_addProtocol(cls, proto);
    }
    objc_registerClassPair(cls);
    Ok(cls)
}

/// `-[SCStreamOutput stream:didOutputSampleBuffer:ofType:]`, on the frame
/// queue. Core Animation allows this off the main thread inside an explicit
/// transaction, on a layer AppKit doesn't manage.
unsafe extern "C" fn did_output(
    _this: Id,
    _cmd: Sel,
    _stream: Id,
    sample: *const c_void,
    kind: isize,
) {
    if kind != 0 || sample.is_null() {
        return; // not a screen frame
    }
    let pixels = CMSampleBufferGetImageBuffer(sample);
    if pixels.is_null() {
        return; // idle frame: nothing changed on the display
    }
    let surface = CVPixelBufferGetIOSurface(pixels) as Id;
    let layer = LAYER.load(Ordering::SeqCst);
    if surface.is_null() || layer.is_null() {
        return;
    }
    let _pool = AutoreleasePool::new();
    let Some(tx) = class("CATransaction") else {
        return;
    };
    msg![(); tx, "begin"];
    msg![(); tx, "setDisableActions:", 1i8 => i8];
    msg![(); layer, "setContents:", surface => Id];
    // A recycled surface looks unchanged to Core Animation; mark it dirty.
    if LAST_SURFACE.swap(surface, Ordering::SeqCst) == surface {
        msg![(); layer, "setContentsChanged"];
    }
    msg![(); tx, "commit"];
    // This thread has no run loop to flush implicit transactions later.
    msg![(); tx, "flush"];
    FRAMES.fetch_add(1, Ordering::SeqCst);
    if STATS.load(Ordering::Relaxed) {
        let pts = CMSampleBufferGetPresentationTimeStamp(sample);
        if pts.flags & kCMTimeFlags_Valid != 0 && pts.timescale > 0 {
            let captured = pts.value as f64 / pts.timescale as f64;
            let us = ((CACurrentMediaTime() - captured).max(0.0) * 1e6) as u64;
            LATENCY_SUM_US.fetch_add(us, Ordering::SeqCst);
            LATENCY_MAX_US.fetch_max(us, Ordering::SeqCst);
        }
    }
}

// ------------------------------------------------------------ blocks

/// Objective-C block literal (Clang block ABI) without captures.
#[repr(C)]
struct Block {
    isa: *const c_void,
    flags: i32,
    reserved: i32,
    invoke: *const c_void,
    descriptor: *const BlockDescriptor,
}

#[repr(C)]
struct BlockDescriptor {
    reserved: usize,
    size: usize,
}

static BLOCK_DESCRIPTOR: BlockDescriptor = BlockDescriptor {
    reserved: 0,
    size: size_of::<Block>(),
};

/// BLOCK_IS_GLOBAL: copying returns the same block, releasing is a no-op.
const BLOCK_IS_GLOBAL: i32 = 1 << 28;

/// A global block calling `invoke` (its first argument is the block itself).
/// Leaked on purpose: the callee may call it at any later point.
fn global_block(invoke: *const c_void) -> Id {
    let block = Box::new(Block {
        isa: unsafe { &_NSConcreteGlobalBlock as *const Opaque as *const c_void },
        flags: BLOCK_IS_GLOBAL,
        reserved: 0,
        invoke,
        descriptor: &BLOCK_DESCRIPTOR,
    });
    Box::into_raw(block) as Id
}
