//! Raw FFI bindings: Objective-C runtime, CoreFoundation, CoreGraphics,
//! ApplicationServices (Accessibility) and libdispatch.
//!
//! Kept deliberately small and hand-written so there is no dependency on the
//! API churn of the objc2 / core-foundation crates.
#![allow(non_upper_case_globals, non_snake_case, dead_code)]

use std::ffi::{c_char, c_void};

pub type Id = *mut c_void;
pub type Sel = *const c_void;
pub type CFTypeRef = *const c_void;
pub type CFStringRef = *const c_void;
pub type CFArrayRef = *const c_void;
pub type CFDictionaryRef = *const c_void;
pub type CFBooleanRef = *const c_void;
pub type AXUIElementRef = *const c_void;
pub type AXValueRef = *const c_void;
pub type CGDirectDisplayID = u32;
pub type CGDisplayConfigRef = *mut c_void;
pub type CGDisplayModeRef = *mut c_void;
pub type CGError = i32;
pub type AXError = i32;

/// Opaque extern symbol; only its address is ever used.
#[repr(C)]
pub struct Opaque {
    _p: [u8; 0],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CGPoint {
    pub x: f64,
    pub y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CGSize {
    pub width: f64,
    pub height: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CGRect {
    pub origin: CGPoint,
    pub size: CGSize,
}

impl CGRect {
    pub fn contains(&self, p: CGPoint) -> bool {
        p.x >= self.origin.x
            && p.y >= self.origin.y
            && p.x < self.origin.x + self.size.width
            && p.y < self.origin.y + self.size.height
    }
}

// ---------------------------------------------------------------- objc runtime
#[cfg_attr(target_os = "macos", link(name = "objc"))]
extern "C" {
    pub fn objc_getClass(name: *const c_char) -> Id;
    pub fn sel_registerName(name: *const c_char) -> Sel;
    /// Never call directly: always cast to the exact signature (see `msg!`).
    pub fn objc_msgSend();
    /// x86_64 only: messages returning structs larger than 16 bytes (see `msg_rect`).
    #[cfg(target_arch = "x86_64")]
    pub fn objc_msgSend_stret();
    pub fn objc_autoreleasePoolPush() -> *mut c_void;
    pub fn objc_autoreleasePoolPop(pool: *mut c_void);
    pub fn objc_allocateClassPair(superclass: Id, name: *const c_char, extra: usize) -> Id;
    pub fn objc_registerClassPair(cls: Id);
    pub fn class_addMethod(cls: Id, name: Sel, imp: *const c_void, types: *const c_char) -> i8;
    pub fn objc_getProtocol(name: *const c_char) -> *const c_void;
    pub fn class_addProtocol(cls: Id, protocol: *const c_void) -> i8;
}

// Foundation + AppKit are only linked so their classes are registered
// (NSArray, NSURL, NSWorkspace, ...).
#[cfg_attr(target_os = "macos", link(name = "Foundation", kind = "framework"))]
extern "C" {}

#[cfg_attr(target_os = "macos", link(name = "AppKit", kind = "framework"))]
extern "C" {
    pub static NSWorkspaceLaunchConfigurationArguments: Id;
}

// ------------------------------------------------------------- CoreFoundation
pub const kCFStringEncodingUTF8: u32 = 0x0800_0100;

#[cfg_attr(target_os = "macos", link(name = "CoreFoundation", kind = "framework"))]
extern "C" {
    pub static kCFBooleanTrue: CFBooleanRef;
    pub static kCFBooleanFalse: CFBooleanRef;
    pub static kCFRunLoopDefaultMode: CFStringRef;
    pub static kCFTypeDictionaryKeyCallBacks: Opaque;
    pub static kCFTypeDictionaryValueCallBacks: Opaque;

    pub fn CFRetain(cf: CFTypeRef) -> CFTypeRef;
    pub fn CFRelease(cf: CFTypeRef);
    pub fn CFGetTypeID(cf: CFTypeRef) -> usize;
    pub fn CFBooleanGetTypeID() -> usize;
    pub fn CFBooleanGetValue(b: CFBooleanRef) -> u8;
    pub fn CFStringGetTypeID() -> usize;
    pub fn CFStringCreateWithCString(
        alloc: *const c_void,
        s: *const c_char,
        enc: u32,
    ) -> CFStringRef;
    pub fn CFStringGetCString(s: CFStringRef, buf: *mut c_char, size: isize, enc: u32) -> u8;
    pub fn CFArrayGetCount(a: CFArrayRef) -> isize;
    pub fn CFArrayGetValueAtIndex(a: CFArrayRef, idx: isize) -> *const c_void;
    pub fn CFDictionaryCreate(
        alloc: *const c_void,
        keys: *const *const c_void,
        values: *const *const c_void,
        count: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> CFDictionaryRef;
    pub fn CFRunLoopRunInMode(mode: CFStringRef, seconds: f64, return_after_source: u8) -> i32;
}

// --------------------------------------------------------------- CoreGraphics
pub const kCGConfigureForAppOnly: u32 = 0;
pub const kCGEventSourceStateHIDSystemState: i32 = 1;
pub const kCGHIDEventTap: u32 = 0;
pub const kCGEventMouseMoved: u32 = 5;
pub const kCGEventLeftMouseDragged: u32 = 6;
pub const kCGEventRightMouseDragged: u32 = 7;
pub const kCGEventOtherMouseDragged: u32 = 27;
/// IOGraphicsTypes.h: the panel's native resolution.
pub const kDisplayModeNativeFlag: u32 = 0x0200_0000;

#[cfg_attr(target_os = "macos", link(name = "CoreGraphics", kind = "framework"))]
extern "C" {
    pub static kCGDisplayShowDuplicateLowResolutionModes: CFStringRef;

    pub fn CGMainDisplayID() -> CGDirectDisplayID;
    pub fn CGGetActiveDisplayList(
        max: u32,
        displays: *mut CGDirectDisplayID,
        count: *mut u32,
    ) -> CGError;
    pub fn CGGetOnlineDisplayList(
        max: u32,
        displays: *mut CGDirectDisplayID,
        count: *mut u32,
    ) -> CGError;
    pub fn CGDisplayBounds(display: CGDirectDisplayID) -> CGRect;
    pub fn CGDisplayIsBuiltin(display: CGDirectDisplayID) -> u32;
    pub fn CGDisplayVendorNumber(display: CGDirectDisplayID) -> u32;
    pub fn CGDisplayMirrorsDisplay(display: CGDirectDisplayID) -> CGDirectDisplayID;
    pub fn CGDisplayCopyDisplayMode(display: CGDirectDisplayID) -> CGDisplayModeRef;
    pub fn CGDisplayModeGetPixelWidth(mode: CGDisplayModeRef) -> usize;
    pub fn CGDisplayModeGetPixelHeight(mode: CGDisplayModeRef) -> usize;
    pub fn CGDisplayModeGetRefreshRate(mode: CGDisplayModeRef) -> f64;
    pub fn CGDisplayModeGetWidth(mode: CGDisplayModeRef) -> usize;
    pub fn CGDisplayModeGetHeight(mode: CGDisplayModeRef) -> usize;
    pub fn CGDisplayModeGetIOFlags(mode: CGDisplayModeRef) -> u32;
    pub fn CGDisplayModeRelease(mode: CGDisplayModeRef);
    pub fn CGDisplayCopyAllDisplayModes(
        display: CGDirectDisplayID,
        options: CFDictionaryRef,
    ) -> CFArrayRef;
    pub fn CGDisplayScreenSize(display: CGDirectDisplayID) -> CGSize;
    pub fn CGEventCreate(source: *const c_void) -> CFTypeRef;
    pub fn CGEventGetLocation(event: CFTypeRef) -> CGPoint;
    pub fn CGWarpMouseCursorPosition(point: CGPoint) -> CGError;
    pub fn CGEventSourceCreate(state: i32) -> CFTypeRef;
    pub fn CGEventSourceSetLocalEventsSuppressionInterval(source: CFTypeRef, seconds: f64);
    pub fn CGEventSourceButtonState(state: i32, button: u32) -> bool;
    pub fn CGEventCreateMouseEvent(
        source: CFTypeRef,
        kind: u32,
        point: CGPoint,
        button: u32,
    ) -> CFTypeRef;
    pub fn CGEventPost(tap: u32, event: CFTypeRef);
    pub fn CGAssociateMouseAndMouseCursorPosition(connected: u32) -> CGError;
    pub fn CGPreflightScreenCaptureAccess() -> bool;
    pub fn CGRequestScreenCaptureAccess() -> bool;
    pub fn CGBeginDisplayConfiguration(config: *mut CGDisplayConfigRef) -> CGError;
    pub fn CGConfigureDisplayMirrorOfDisplay(
        config: CGDisplayConfigRef,
        display: CGDirectDisplayID,
        master: CGDirectDisplayID,
    ) -> CGError;
    pub fn CGCompleteDisplayConfiguration(config: CGDisplayConfigRef, option: u32) -> CGError;
    pub fn CGCancelDisplayConfiguration(config: CGDisplayConfigRef) -> CGError;
}

// ----------------------------------------------- ApplicationServices (AX API)
pub const kAXErrorSuccess: AXError = 0;
pub const kAXValueCGPointType: u32 = 1;
pub const kAXValueCGSizeType: u32 = 2;

#[cfg_attr(
    target_os = "macos",
    link(name = "ApplicationServices", kind = "framework")
)]
extern "C" {
    pub static kAXTrustedCheckOptionPrompt: CFStringRef;
    pub fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> u8;
    pub fn AXUIElementGetTypeID() -> usize;
    pub fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    pub fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: *mut CFTypeRef,
    ) -> AXError;
    pub fn AXUIElementSetAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: CFTypeRef,
    ) -> AXError;
    pub fn AXUIElementPerformAction(element: AXUIElementRef, action: CFStringRef) -> AXError;
    pub fn AXValueCreate(the_type: u32, value: *const c_void) -> AXValueRef;
    pub fn AXValueGetValue(value: AXValueRef, the_type: u32, out: *mut c_void) -> u8;
}

// -------------------------------------------------- CoreMedia / CoreVideo
/// CMTime, passed by value to `setMinimumFrameInterval:`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CMTime {
    pub value: i64,
    pub timescale: i32,
    pub flags: u32,
    pub epoch: i64,
}
pub const kCMTimeFlags_Valid: u32 = 1;

#[cfg_attr(target_os = "macos", link(name = "CoreMedia", kind = "framework"))]
extern "C" {
    pub fn CMSampleBufferGetImageBuffer(sample: *const c_void) -> *const c_void;
    pub fn CMSampleBufferGetPresentationTimeStamp(sample: *const c_void) -> CMTime;
}

#[cfg_attr(target_os = "macos", link(name = "QuartzCore", kind = "framework"))]
extern "C" {
    /// Host time in seconds, the clock ScreenCaptureKit timestamps use.
    pub fn CACurrentMediaTime() -> f64;
}

#[cfg_attr(target_os = "macos", link(name = "CoreVideo", kind = "framework"))]
extern "C" {
    pub fn CVPixelBufferGetIOSurface(pixel_buffer: *const c_void) -> *const c_void;
}

// ------------------------------------------------------ libdispatch / libc
pub const RTLD_NOW: i32 = 2;
/// `RTLD_DEFAULT` on macOS: search every loaded image.
pub const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;
pub const QOS_CLASS_USER_INTERACTIVE: u32 = 0x21;
pub const LOCK_EX: i32 = 2;
pub const LOCK_NB: i32 = 4;

extern "C" {
    /// `dispatch_get_main_queue()` is a macro for `&_dispatch_main_q`.
    pub static _dispatch_main_q: Opaque;
    /// `attr` null = DISPATCH_QUEUE_SERIAL.
    pub fn dispatch_queue_attr_make_with_qos_class(
        attr: *const c_void,
        qos_class: u32,
        relative_priority: i32,
    ) -> *const c_void;
    pub fn dispatch_queue_create(label: *const c_char, attr: *const c_void) -> Id;
    pub fn dispatch_sync_f(queue: Id, context: *mut c_void, work: extern "C" fn(*mut c_void));
    pub fn dispatch_release(object: Id);
    /// isa of a global (static, never copied) Objective-C block.
    pub static _NSConcreteGlobalBlock: Opaque;
    pub fn kill(pid: i32, sig: i32) -> i32;
    pub fn getuid() -> u32;
    pub fn getppid() -> i32;
    /// Returns the previous handler (`sig_t`), unused.
    pub fn signal(sig: i32, handler: extern "C" fn(i32)) -> usize;
    pub fn flock(fd: i32, operation: i32) -> i32;
    pub fn dlopen(path: *const c_char, mode: i32) -> *mut c_void;
    pub fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

pub fn dispatch_main_queue() -> Id {
    unsafe { &_dispatch_main_q as *const Opaque as Id }
}
