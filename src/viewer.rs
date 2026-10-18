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
use std::time::{Duration, Instant};

/// Layer the frames are drawn into; null when no viewer is open.
static LAYER: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
/// Last IOSurface shown, to force a redraw when ScreenCaptureKit reuses one.
static LAST_SURFACE: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static FRAMES: AtomicU64 = AtomicU64::new(0);

/// Where the real pointer is, and its last known position.
#[derive(Clone, Copy, Debug)]
enum Pointer {
    /// On a real screen.
    Outside(CGPoint),
    /// Moved onto the virtual display because it entered the viewer.
    Inside(CGPoint),
}

pub struct Viewer {
    window: Id,
    stream: Id,
    output: Id,
    /// The window's content, in global CoreGraphics coordinates.
    rect: CGRect,
    display: CGDirectDisplayID,
    pointer: Pointer,
}

impl Viewer {
    /// Shows virtual display `display` (`pixels` = its backing size) in a
    /// window covering `rect`.
    pub fn start(display: CGDirectDisplayID, pixels: (u32, u32), rect: CGRect) -> Result<Self> {
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

            let mut viewer = Viewer {
                window: create_window(rect)?,
                stream: ptr::null_mut(),
                output: ptr::null_mut(),
                rect,
                display,
                pointer: Pointer::Outside(mouse_location()),
            };
            let sc_display = find_sc_display(display)?;
            let started = start_stream(sc_display, pixels);
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
            Ok(viewer)
        }
    }

    /// Moves the pointer between the viewer and the virtual display. Call it
    /// every few milliseconds.
    pub fn tick(&mut self) {
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
                pump(Duration::from_millis(200)); // let queued frames drain
            }
            release(self.stream);
            release(self.output);
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

/// Borderless, shadowless window over `rect` that draws its layer's contents
/// scaled to fit. Shown without activating vdisplay.
unsafe fn create_window(rect: CGRect) -> Result<Id> {
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
    let gravity = CfStr::new("resizeAspect");
    msg![(); layer, "setContentsGravity:", gravity.as_id() => Id];
    LAYER.store(layer, Ordering::SeqCst);
    msg![(); window, "orderFrontRegardless"];
    Ok(window)
}

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

/// Starts capturing `sc_display` at `pixels`, frames delivered on the main
/// queue. Returns the stream and its output object (both +1 retained).
unsafe fn start_stream(sc_display: Id, pixels: (u32, u32)) -> Result<(Id, Id)> {
    let empty: Id = msg![Id; class("NSArray").context("NSArray missing")?, "array"];
    let filter: Id = msg![Id; msg![Id; class("SCContentFilter").context("SCContentFilter missing")?, "alloc"],
        "initWithDisplay:excludingWindows:", sc_display => Id, empty => Id];
    let config =
        alloc_init(class("SCStreamConfiguration").context("SCStreamConfiguration missing")?);
    msg![(); config, "setWidth:", pixels.0 as usize => usize];
    msg![(); config, "setHeight:", pixels.1 as usize => usize];
    msg![(); config, "setPixelFormat:", u32::from_be_bytes(*b"BGRA") => u32];
    msg![(); config, "setShowsCursor:", 1i8 => i8];
    msg![(); config, "setQueueDepth:", 5isize => isize];
    let interval = CMTime {
        value: 1,
        timescale: 60,
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
        output => Id, 0isize => isize, dispatch_main_queue() => Id, &mut err as *mut Id => *mut Id];
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

/// `-[SCStreamOutput stream:didOutputSampleBuffer:ofType:]`, on the main queue.
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
    FRAMES.fetch_add(1, Ordering::SeqCst);
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
