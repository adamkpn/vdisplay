//! Virtual display creation through CoreGraphics' private `CGVirtualDisplay`
//! classes (the same API used by DeskPad / BetterDisplay, macOS 11+).

use crate::ffi::*;
use crate::msg;
use crate::objc::*;
use anyhow::{bail, Context, Result};
use std::ffi::c_void;
use std::ptr;
use std::time::{Duration, Instant};

pub struct DisplayConfig {
    pub name: String,
    /// Logical size in points — what applications see as the screen size.
    pub width: u32,
    pub height: u32,
    pub refresh: f64,
    /// Back the display with 2x pixels (Retina). Pixel size = 2 * logical size.
    pub hidpi: bool,
    /// Pixel density used to compute the reported physical size.
    pub ppi: f64,
}

/// Vendor ID of displays vdisplay creates (`DisplayInfo::ours`).
pub const VENDOR_ID: u32 = 0xF0F0;

pub struct VirtualDisplay {
    display: Id,
    descriptor: Id,
    pub id: CGDirectDisplayID,
}

impl VirtualDisplay {
    pub fn create(cfg: &DisplayConfig) -> Result<Self> {
        unsafe {
            let _pool = AutoreleasePool::new();
            let missing = "CGVirtualDisplay private API not found (requires macOS 11 or newer)";
            let desc_cls = class("CGVirtualDisplayDescriptor").context(missing)?;
            let disp_cls = class("CGVirtualDisplay").context(missing)?;
            let settings_cls = class("CGVirtualDisplaySettings").context(missing)?;
            let mode_cls = class("CGVirtualDisplayMode").context(missing)?;
            let array_cls = class("NSArray").context("NSArray missing")?;

            let scale = if cfg.hidpi { 2 } else { 1 };
            let (px_w, px_h) = (cfg.width * scale, cfg.height * scale);

            // --- descriptor
            let descriptor = alloc_init(desc_cls);
            let name = CfStr::new(&cfg.name);
            msg![(); descriptor, "setQueue:", dispatch_main_queue() => Id];
            msg![(); descriptor, "setName:", name.as_id() => Id];
            msg![(); descriptor, "setMaxPixelsWide:", px_w => u32];
            msg![(); descriptor, "setMaxPixelsHigh:", px_h => u32];
            let mm = CGSize {
                width: px_w as f64 / cfg.ppi * 25.4,
                height: px_h as f64 / cfg.ppi * 25.4,
            };
            msg![(); descriptor, "setSizeInMillimeters:", mm => CGSize];
            // Stable IDs per resolution so macOS remembers the arrangement.
            msg![(); descriptor, "setVendorID:", VENDOR_ID => u32];
            msg![(); descriptor, "setProductID:", 0x1D15u32 => u32];
            let serial = px_w.wrapping_mul(10_007) ^ px_h ^ (scale << 30);
            msg![(); descriptor, "setSerialNum:", serial => u32];

            // --- display
            let display: Id =
                msg![Id; msg![Id; disp_cls, "alloc"], "initWithDescriptor:", descriptor => Id];
            if display.is_null() {
                release(descriptor);
                bail!("CGVirtualDisplay initWithDescriptor: returned nil");
            }

            // --- mode + settings
            let settings = alloc_init(settings_cls);
            msg![(); settings, "setHiDPI:", cfg.hidpi as u32 => u32];
            let mode: Id = msg![Id; msg![Id; mode_cls, "alloc"],
                "initWithWidth:height:refreshRate:",
                cfg.width => u32, cfg.height => u32, cfg.refresh => f64];
            let modes: Id = msg![Id; array_cls, "arrayWithObject:", mode => Id];
            msg![(); settings, "setModes:", modes => Id];
            let ok: i8 = msg![i8; display, "applySettings:", settings => Id];
            release(mode);
            release(settings);

            let id: u32 = msg![u32; display, "displayID"];
            let vd = VirtualDisplay {
                display,
                descriptor,
                id,
            };
            if ok == 0 {
                bail!(
                    "applySettings failed for {}x{} ({}x{} px). The resolution may exceed what \
                     your GPU / macOS version allows for a virtual display.",
                    cfg.width,
                    cfg.height,
                    px_w,
                    px_h
                );
            }
            if id == 0 {
                bail!("virtual display was created but has no display ID");
            }
            Ok(vd)
        }
    }

    /// Waits until the display shows up in the active display list.
    pub fn wait_online(&self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if active_displays().contains(&self.id) {
                // Give WindowServer a moment to finish the arrangement.
                pump(Duration::from_millis(500));
                return Ok(());
            }
            pump(Duration::from_millis(100));
        }
        bail!(
            "virtual display {} did not come online within {:?}",
            self.id,
            timeout
        )
    }

    pub fn bounds(&self) -> CGRect {
        unsafe { CGDisplayBounds(self.id) }
    }
}

impl Drop for VirtualDisplay {
    fn drop(&mut self) {
        // Releasing the CGVirtualDisplay removes it; macOS moves its windows
        // back to a physical screen.
        unsafe {
            release(self.display);
            release(self.descriptor);
        }
    }
}

pub fn active_displays() -> Vec<CGDirectDisplayID> {
    let mut ids = [0u32; 32];
    let mut count = 0u32;
    let err = unsafe { CGGetActiveDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut count) };
    if err != 0 {
        return Vec::new();
    }
    ids[..count as usize].to_vec()
}

pub struct DisplayInfo {
    pub id: CGDirectDisplayID,
    pub bounds: CGRect,
    pub pixels: (usize, usize),
    pub refresh: f64,
    pub builtin: bool,
    pub main: bool,
    pub mirrors: Option<CGDirectDisplayID>,
    /// A virtual display made by vdisplay.
    pub ours: bool,
}

pub fn list_displays() -> Vec<DisplayInfo> {
    let mut ids = [0u32; 32];
    let mut count = 0u32;
    unsafe {
        if CGGetOnlineDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut count) != 0 {
            return Vec::new();
        }
        let main = CGMainDisplayID();
        ids[..count as usize]
            .iter()
            .map(|&id| {
                let mode = CGDisplayCopyDisplayMode(id);
                let (pixels, refresh) = if mode.is_null() {
                    ((0, 0), 0.0)
                } else {
                    let r = (
                        (
                            CGDisplayModeGetPixelWidth(mode),
                            CGDisplayModeGetPixelHeight(mode),
                        ),
                        CGDisplayModeGetRefreshRate(mode),
                    );
                    CGDisplayModeRelease(mode);
                    r
                };
                let m = CGDisplayMirrorsDisplay(id);
                DisplayInfo {
                    id,
                    bounds: CGDisplayBounds(id),
                    pixels,
                    refresh,
                    builtin: CGDisplayIsBuiltin(id) != 0,
                    main: id == main,
                    mirrors: (m != 0).then_some(m),
                    ours: CGDisplayVendorNumber(id) == VENDOR_ID,
                }
            })
            .collect()
    }
}

/// The panel's native resolution in pixels, which can differ from the current
/// mode (e.g. a Retina MacBook running "looks like 1512x982"). Prefers the mode
/// macOS flags as native, else the largest 1x mode: scaled HiDPI modes can
/// report a backing size above the panel's.
pub fn native_resolution(id: CGDirectDisplayID) -> Option<(usize, usize)> {
    unsafe {
        let keys = [kCGDisplayShowDuplicateLowResolutionModes];
        let vals = [kCFBooleanTrue];
        let opts = CFDictionaryCreate(
            ptr::null(),
            keys.as_ptr(),
            vals.as_ptr(),
            1,
            &kCFTypeDictionaryKeyCallBacks as *const Opaque as *const c_void,
            &kCFTypeDictionaryValueCallBacks as *const Opaque as *const c_void,
        );
        let modes = CGDisplayCopyAllDisplayModes(id, opts);
        if !opts.is_null() {
            CFRelease(opts);
        }
        if modes.is_null() {
            return None;
        }
        let area = |p: &(usize, usize)| p.0 * p.1;
        let (mut native, mut largest_1x) = (None, None);
        for i in 0..CFArrayGetCount(modes) {
            let m = CFArrayGetValueAtIndex(modes, i) as CGDisplayModeRef;
            let px = (
                CGDisplayModeGetPixelWidth(m),
                CGDisplayModeGetPixelHeight(m),
            );
            let bigger =
                |cur: &Option<(usize, usize)>| cur.as_ref().is_none_or(|c| area(&px) > area(c));
            if CGDisplayModeGetIOFlags(m) & kDisplayModeNativeFlag != 0 && bigger(&native) {
                native = Some(px);
            }
            let pts = (CGDisplayModeGetWidth(m), CGDisplayModeGetHeight(m));
            if pts == px && bigger(&largest_1x) {
                largest_1x = Some(px);
            }
        }
        CFRelease(modes);
        native.or(largest_1x)
    }
}

/// The part of display `id` windows may use (no menu bar, no Dock), in global
/// CoreGraphics coordinates: points, origin at the top-left of the main display.
pub fn usable_bounds(id: CGDirectDisplayID) -> Option<CGRect> {
    with_nsscreen(id, |screen| unsafe {
        // Cocoa measures y upwards from the bottom of the main display.
        let f = msg_rect(screen, "visibleFrame");
        let main_h = CGDisplayBounds(CGMainDisplayID()).size.height;
        CGRect {
            origin: CGPoint {
                x: f.origin.x,
                y: main_h - f.origin.y - f.size.height,
            },
            size: f.size,
        }
    })
}

/// Backing pixels per point of display `id` (2.0 on Retina screens).
pub fn backing_scale(id: CGDirectDisplayID) -> Option<f64> {
    with_nsscreen(id, |screen| unsafe {
        msg![f64; screen, "backingScaleFactor"]
    })
}

/// Runs `f` on the `NSScreen` of display `id`, if it has one.
fn with_nsscreen<T>(id: CGDirectDisplayID, f: impl FnOnce(Id) -> T) -> Option<T> {
    unsafe {
        let _pool = AutoreleasePool::new();
        let screens: Id = msg![Id; class("NSScreen")?, "screens"];
        if screens.is_null() {
            return None;
        }
        let key = CfStr::new("NSScreenNumber");
        for i in 0..msg![usize; screens, "count"] {
            let screen: Id = msg![Id; screens, "objectAtIndex:", i => usize];
            let desc: Id = msg![Id; screen, "deviceDescription"];
            let num: Id = msg![Id; desc, "objectForKey:", key.as_id() => Id];
            if !num.is_null() && msg![u32; num, "unsignedIntValue"] == id {
                return Some(f(screen));
            }
        }
        None
    }
}

/// Physical size reported by the display (EDID), if it reports one.
pub fn screen_size_mm(id: CGDirectDisplayID) -> Option<CGSize> {
    let mm = unsafe { CGDisplayScreenSize(id) };
    (mm.width > 0.0 && mm.height > 0.0).then_some(mm)
}

/// Make `display` (a physical screen) show the content of `master` (the
/// virtual display), scaled to fit. Uses app-only scope, so macOS reverts it
/// automatically when this process exits — even if it crashes.
pub fn mirror(display: CGDirectDisplayID, master: CGDirectDisplayID) -> Result<()> {
    unsafe {
        let mut cfg: CGDisplayConfigRef = ptr::null_mut();
        let e = CGBeginDisplayConfiguration(&mut cfg);
        if e != 0 {
            bail!("CGBeginDisplayConfiguration failed ({e})");
        }
        let e = CGConfigureDisplayMirrorOfDisplay(cfg, display, master);
        if e != 0 {
            CGCancelDisplayConfiguration(cfg);
            bail!("CGConfigureDisplayMirrorOfDisplay({display} -> {master}) failed ({e})");
        }
        let e = CGCompleteDisplayConfiguration(cfg, kCGConfigureForAppOnly);
        if e != 0 {
            bail!("CGCompleteDisplayConfiguration failed ({e})");
        }
    }
    Ok(())
}
