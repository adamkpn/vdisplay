//! Tiny Objective-C / CoreFoundation helpers on top of the raw FFI.

use crate::ffi::*;
use std::ffi::{c_char, c_void, CStr, CString};
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::time::{Duration, Instant};

/// Send an Objective-C message with an explicit signature.
///
/// `msg![RetType; receiver, "selector:with:", arg1 => Type1, arg2 => Type2]`
///
/// `objc_msgSend` must be called through a pointer cast to the exact C
/// signature (mandatory on Apple Silicon), which is what this macro does.
#[macro_export]
macro_rules! msg {
    ($ret:ty; $obj:expr, $sel:expr $(, $arg:expr => $t:ty)* $(,)?) => {{
        let f: unsafe extern "C" fn($crate::ffi::Id, $crate::ffi::Sel $(, $t)*) -> $ret =
            std::mem::transmute($crate::ffi::objc_msgSend as unsafe extern "C" fn());
        f($obj, $crate::objc::sel($sel) $(, $arg)*)
    }};
}

/// Send a message that returns an `NSRect` / `CGRect`. On x86_64 a 32-byte
/// struct comes back through `objc_msgSend_stret`; arm64 has no stret variant.
pub unsafe fn msg_rect(obj: Id, selector: &str) -> CGRect {
    #[cfg(target_arch = "x86_64")]
    let send = objc_msgSend_stret as unsafe extern "C" fn();
    #[cfg(not(target_arch = "x86_64"))]
    let send = objc_msgSend as unsafe extern "C" fn();
    let f: unsafe extern "C" fn(Id, Sel) -> CGRect = std::mem::transmute(send);
    f(obj, sel(selector))
}

pub fn class(name: &str) -> Option<Id> {
    let c = CString::new(name).ok()?;
    let p = unsafe { objc_getClass(c.as_ptr()) };
    (!p.is_null()).then_some(p)
}

pub fn sel(name: &str) -> Sel {
    let c = CString::new(name).expect("selector contains NUL");
    unsafe { sel_registerName(c.as_ptr()) }
}

/// `[[Class alloc] init]` (+1 retained).
pub unsafe fn alloc_init(cls: Id) -> Id {
    let obj: Id = msg![Id; cls, "alloc"];
    msg![Id; obj, "init"]
}

pub unsafe fn release(obj: Id) {
    if !obj.is_null() {
        msg![(); obj, "release"];
    }
}

/// Converts an `NSString*` (or `CFStringRef`) to a Rust `String`.
pub unsafe fn nsstring_to_string(s: Id) -> Option<String> {
    if s.is_null() {
        return None;
    }
    let p: *const c_char = msg![*const c_char; s, "UTF8String"];
    (!p.is_null()).then(|| CStr::from_ptr(p).to_string_lossy().into_owned())
}

/// Autoreleased `NSString*` (call inside an autorelease pool).
pub unsafe fn ns_string(s: &str) -> Id {
    let c = CString::new(s.replace('\0', "")).unwrap_or_default();
    let Some(cls) = class("NSString") else {
        return ptr::null_mut();
    };
    msg![Id; cls, "stringWithUTF8String:", c.as_ptr() => *const c_char]
}

/// Owned `CFStringRef`, toll-free bridged to `NSString*`.
pub struct CfStr(pub CFStringRef);

impl CfStr {
    pub fn new(s: &str) -> Self {
        let c = CString::new(s).expect("string contains NUL");
        let r =
            unsafe { CFStringCreateWithCString(ptr::null(), c.as_ptr(), kCFStringEncodingUTF8) };
        assert!(!r.is_null(), "CFStringCreateWithCString failed");
        CfStr(r)
    }
    pub fn as_id(&self) -> Id {
        self.0 as Id
    }
}

impl Drop for CfStr {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) }
    }
}

/// RAII autorelease pool.
pub struct AutoreleasePool(*mut std::ffi::c_void);

impl AutoreleasePool {
    pub fn new() -> Self {
        AutoreleasePool(unsafe { objc_autoreleasePoolPush() })
    }
}

impl Drop for AutoreleasePool {
    fn drop(&mut self) {
        unsafe { objc_autoreleasePoolPop(self.0) }
    }
}

/// NSApplication whose events `pump` drains; null until a window is shown.
static EVENT_APP: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());

/// From now on `pump` also reads AppKit events for `app`. A process with a
/// window that stops reading events gets the spinning cursor over it.
pub fn set_event_app(app: Id) {
    EVENT_APP.store(app, Ordering::SeqCst);
}

/// Pump the main run loop for `d` (virtual display callbacks are delivered on
/// the main dispatch queue, so the main thread must keep spinning).
pub fn pump(d: Duration) {
    let app = EVENT_APP.load(Ordering::SeqCst);
    let Some(date_cls) = (!app.is_null()).then(|| class("NSDate")).flatten() else {
        unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, d.as_secs_f64(), 0) };
        return;
    };
    let deadline = Instant::now() + d;
    loop {
        let _pool = AutoreleasePool::new();
        let left = deadline
            .saturating_duration_since(Instant::now())
            .as_secs_f64();
        unsafe {
            let until: Id = msg![Id; date_cls, "dateWithTimeIntervalSinceNow:", left => f64];
            let event: Id = msg![Id; app, "nextEventMatchingMask:untilDate:inMode:dequeue:",
                u64::MAX => u64, until => Id, kCFRunLoopDefaultMode as Id => Id, 1i8 => i8];
            if event.is_null() {
                return; // deadline reached
            }
            // Dropping clicks keeps vdisplay from activating and taking keyboard
            // focus from the app if one lands on the viewer window.
            let kind: usize = msg![usize; event, "type"];
            if !matches!(kind, 1..=4 | 25 | 26) {
                msg![(); app, "sendEvent:", event => Id];
            }
        }
        if Instant::now() >= deadline {
            return;
        }
    }
}
