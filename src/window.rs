//! Window discovery and placement through the Accessibility (AX) API.

use crate::ffi::*;
use crate::objc::{pump, CfStr};
use anyhow::{bail, Result};
use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Returns whether this process (in practice: your terminal app) may control
/// other apps' windows. With `prompt`, macOS shows the permission dialog.
pub fn is_trusted(prompt: bool) -> bool {
    unsafe {
        let keys = [kAXTrustedCheckOptionPrompt];
        let vals = [if prompt {
            kCFBooleanTrue
        } else {
            kCFBooleanFalse
        }];
        let dict = CFDictionaryCreate(
            ptr::null(),
            keys.as_ptr(),
            vals.as_ptr(),
            1,
            &kCFTypeDictionaryKeyCallBacks as *const Opaque as *const c_void,
            &kCFTypeDictionaryValueCallBacks as *const Opaque as *const c_void,
        );
        let ok = AXIsProcessTrustedWithOptions(dict) != 0;
        CFRelease(dict);
        ok
    }
}

/// Owned AXUIElementRef.
pub struct AxElement(AXUIElementRef);

impl Drop for AxElement {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) }
    }
}

/// Owned generic CFTypeRef.
struct CfOwned(CFTypeRef);

impl Drop for CfOwned {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) }
    }
}

impl AxElement {
    pub fn application(pid: i32) -> Self {
        AxElement(unsafe { AXUIElementCreateApplication(pid) })
    }

    fn copy_attr(&self, name: &str) -> Option<CfOwned> {
        let attr = CfStr::new(name);
        let mut value: CFTypeRef = ptr::null();
        let err = unsafe { AXUIElementCopyAttributeValue(self.0, attr.0, &mut value) };
        (err == kAXErrorSuccess && !value.is_null()).then(|| CfOwned(value))
    }

    fn set_attr(&self, name: &str, value: CFTypeRef) -> Result<()> {
        let attr = CfStr::new(name);
        let err = unsafe { AXUIElementSetAttributeValue(self.0, attr.0, value) };
        if err != kAXErrorSuccess {
            bail!("setting {name} failed (AXError {err})");
        }
        Ok(())
    }

    pub fn perform(&self, action: &str) {
        let a = CfStr::new(action);
        unsafe {
            AXUIElementPerformAction(self.0, a.0);
        }
    }

    fn element_attr(&self, name: &str) -> Option<AxElement> {
        let v = self.copy_attr(name)?;
        unsafe {
            if CFGetTypeID(v.0) != AXUIElementGetTypeID() {
                return None;
            }
            Some(AxElement(CFRetain(v.0)))
        }
    }

    pub fn bool_attr(&self, name: &str) -> Option<bool> {
        let v = self.copy_attr(name)?;
        unsafe { (CFGetTypeID(v.0) == CFBooleanGetTypeID()).then(|| CFBooleanGetValue(v.0) != 0) }
    }

    pub fn string_attr(&self, name: &str) -> Option<String> {
        let v = self.copy_attr(name)?;
        unsafe {
            if CFGetTypeID(v.0) != CFStringGetTypeID() {
                return None;
            }
            let mut buf = [0i8; 512];
            if CFStringGetCString(
                v.0,
                buf.as_mut_ptr() as _,
                buf.len() as isize,
                kCFStringEncodingUTF8,
            ) == 0
            {
                return None;
            }
            Some(
                std::ffi::CStr::from_ptr(buf.as_ptr() as _)
                    .to_string_lossy()
                    .into_owned(),
            )
        }
    }

    pub fn set_bool(&self, name: &str, v: bool) -> Result<()> {
        self.set_attr(name, unsafe {
            if v {
                kCFBooleanTrue
            } else {
                kCFBooleanFalse
            }
        })
    }

    pub fn windows(&self) -> Vec<AxElement> {
        let Some(arr) = self.copy_attr("AXWindows") else {
            return Vec::new();
        };
        unsafe {
            (0..CFArrayGetCount(arr.0))
                .map(|i| CFArrayGetValueAtIndex(arr.0, i))
                .filter(|w| !w.is_null())
                .map(|w| AxElement(CFRetain(w)))
                .collect()
        }
    }

    pub fn position(&self) -> Option<CGPoint> {
        let v = self.copy_attr("AXPosition")?;
        let mut p = CGPoint::default();
        let ok =
            unsafe { AXValueGetValue(v.0, kAXValueCGPointType, &mut p as *mut _ as *mut c_void) };
        (ok != 0).then_some(p)
    }

    pub fn size(&self) -> Option<CGSize> {
        let v = self.copy_attr("AXSize")?;
        let mut s = CGSize::default();
        let ok =
            unsafe { AXValueGetValue(v.0, kAXValueCGSizeType, &mut s as *mut _ as *mut c_void) };
        (ok != 0).then_some(s)
    }

    pub fn set_position(&self, p: CGPoint) -> Result<()> {
        let v =
            CfOwned(unsafe { AXValueCreate(kAXValueCGPointType, &p as *const _ as *const c_void) });
        self.set_attr("AXPosition", v.0)
    }

    pub fn set_size(&self, s: CGSize) -> Result<()> {
        let v =
            CfOwned(unsafe { AXValueCreate(kAXValueCGSizeType, &s as *const _ as *const c_void) });
        self.set_attr("AXSize", v.0)
    }

    pub fn center(&self) -> Option<CGPoint> {
        let (p, s) = (self.position()?, self.size()?);
        Some(CGPoint {
            x: p.x + s.width / 2.0,
            y: p.y + s.height / 2.0,
        })
    }
}

/// Waits for the app to present a usable window and returns it.
/// Preference: focused window → main window → first standard window.
pub fn wait_for_window(
    pid: i32,
    timeout: Duration,
    stop: &AtomicBool,
    alive: impl Fn() -> bool,
) -> Result<AxElement> {
    let app = AxElement::application(pid);
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if stop.load(Ordering::SeqCst) {
            bail!("interrupted");
        }
        if !alive() {
            bail!(
                "the launched process (pid {pid}) exited before showing a window. \
                 Some apps (e.g. Chromium browsers) hand off to an already-running \
                 instance — quit the app first, or use --new-instance with a separate profile."
            );
        }
        let pick = app
            .element_attr("AXFocusedWindow")
            .or_else(|| app.element_attr("AXMainWindow"))
            .or_else(|| {
                let mut ws = app.windows();
                let idx = ws.iter().position(|w| {
                    w.string_attr("AXSubrole").as_deref() == Some("AXStandardWindow")
                });
                idx.map(|i| ws.swap_remove(i))
            });
        if let Some(w) = pick {
            return Ok(w);
        }
        pump(Duration::from_millis(250));
    }
    bail!("no window appeared within {timeout:?}")
}

/// Takes `win` out of native full screen and waits for the animation to end.
pub fn exit_fullscreen(win: &AxElement) -> Result<()> {
    if win.bool_attr("AXFullScreen") != Some(true) {
        return Ok(());
    }
    win.set_bool("AXFullScreen", false)?;
    let deadline = Instant::now() + Duration::from_secs(3);
    while win.bool_attr("AXFullScreen") == Some(true) && Instant::now() < deadline {
        pump(Duration::from_millis(100));
    }
    // The attribute flips before the exit animation is over.
    pump(Duration::from_millis(800));
    Ok(())
}

/// Hands `win` back before its virtual display goes away: out of native full
/// screen, then into `home` if given. Removing a display while a window on it
/// is in full screen makes macOS move that full-screen Space to another
/// display, where the window can no longer be resized.
pub fn release_window(win: &AxElement, home: Option<CGRect>) -> Result<()> {
    match home {
        Some(rect) => place_window(win, rect, false),
        None => exit_fullscreen(win),
    }
}

/// Moves and resizes `win` to fill `bounds` (a whole display or part of one),
/// and optionally switches it to native macOS full screen on that display.
pub fn place_window(win: &AxElement, bounds: CGRect, fullscreen: bool) -> Result<()> {
    exit_fullscreen(win)?;
    if win.bool_attr("AXMinimized") == Some(true) {
        win.set_bool("AXMinimized", false)?;
        pump(Duration::from_millis(500));
    }

    // Move first (macOS clamps the size to the screen the window is on),
    // then resize, then move again to fix any clamping.
    let origin = bounds.origin;
    win.set_position(origin)?;
    pump(Duration::from_millis(300));
    win.set_size(bounds.size)?;
    pump(Duration::from_millis(300));
    win.set_position(origin)?;
    pump(Duration::from_millis(300));
    win.perform("AXRaise");

    match win.center() {
        Some(c) if bounds.contains(c) => {}
        Some(c) => {
            bail!(
                "window did not land in the target area (center at {:.0},{:.0}, area {:?}). \
             Check that the app allows its window to be moved.",
                c.x,
                c.y,
                bounds
            )
        }
        None => eprintln!("warning: could not read back window geometry"),
    }

    if fullscreen {
        if let Err(e) = win.set_bool("AXFullScreen", true) {
            eprintln!(
                "warning: native full screen not supported by this window ({e}); left maximized"
            );
        } else {
            pump(Duration::from_millis(1500)); // full-screen enter animation
        }
    }
    Ok(())
}
