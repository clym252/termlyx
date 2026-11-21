//! iOS-only native adjustments to the webview.
//!
//! WKWebView shows a form accessory bar (previous / next / Done) above the
//! software keyboard whenever a text field has the focus, xterm's hidden
//! textarea included. The terminal has its own key bar (React,
//! `src/terminal/KeyBar.tsx`) in that place, so the accessory bar goes:
//! `-[WKContentView inputAccessoryView]`, which UIKit asks for the view to
//! put above the keyboard, is replaced for the whole process with one that
//! returns nil. Done in Rust (objc2) rather than a Swift file in
//! `gen/apple/Sources`, so the generated Xcode project needs no change.
//!
//! On an iPad the keyboard also carries a shortcut strip (undo / redo /
//! paste, the `inputAssistantItem` bar button groups) in the same place;
//! `-[WKContentView inputAssistantItem]` is wrapped to empty its groups, so
//! the key bar is not doubled by the system's strip.
//!
//! Both apply to every text field in the app, forms included (accepted; how
//! to scope them to the terminal: `docs/ios-dev.md`).
//!
//! A hardware keyboard's keys reach the page only while the webview is the
//! first responder, which it is not until the first touch: at launch and
//! after coming back to the front, [`claim_hardware_keys`] makes it one, so
//! ⌘T, ⌘K and the rest work without tapping the screen first.
//!
//! [`keepalive`]: background keep-alive's location updates.

pub mod keepalive;

use std::ffi::c_char;
use std::sync::OnceLock;

use objc2::ffi::{class_getInstanceMethod, class_replaceMethod, method_getImplementation};
use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
use objc2::{msg_send, sel};

/// WebKit's private content view class: the first responder while a field
/// in the page has the focus.
const CONTENT_VIEW: &std::ffi::CStr = c"WKContentView";

/// Type encoding of `- (UIView *)inputAccessoryView`: returns an object,
/// takes self and _cmd.
const RETURNS_OBJECT: &std::ffi::CStr = c"@@:";

/// The replacement `inputAccessoryView`: no view, so no bar.
unsafe extern "C-unwind" fn no_accessory_view(_this: *mut AnyObject, _cmd: Sel) -> *mut AnyObject {
    std::ptr::null_mut()
}

/// Removes WKWebView's form accessory bar for every webview in the app.
/// Call once, before the keyboard first shows (app setup). If WebKit ever
/// renames the class, this logs and leaves the bar in place.
pub fn hide_form_accessory_bar() {
    let Some(class) = AnyClass::get(CONTENT_VIEW) else {
        tracing::warn!("WKContentView not found; the keyboard's form accessory bar stays");
        return;
    };
    // SAFETY: `no_accessory_view` matches the method's signature (object
    // return, self + _cmd, no further arguments) and `RETURNS_OBJECT` says
    // so; the runtime calls an `Imp` with the method's real arguments, so
    // erasing the function's type here is what `class_replaceMethod` expects.
    unsafe {
        let imp = std::mem::transmute::<
            unsafe extern "C-unwind" fn(*mut AnyObject, Sel) -> *mut AnyObject,
            Imp,
        >(no_accessory_view);
        class_replaceMethod(
            std::ptr::from_ref(class).cast_mut(),
            sel!(inputAccessoryView),
            imp,
            RETURNS_OBJECT.as_ptr().cast::<c_char>(),
        );
    }
}

/// `- (UITextInputAssistantItem *)inputAssistantItem` as it was (UIKit's
/// own, or WebKit's override): returns an object, takes self and _cmd.
type AssistantItemFn = unsafe extern "C-unwind" fn(*mut AnyObject, Sel) -> *mut AnyObject;

static ORIGINAL_ASSISTANT_ITEM: OnceLock<AssistantItemFn> = OnceLock::new();
/// `NSArray`, looked up once in `hide_shortcut_strip`.
static NS_ARRAY: OnceLock<&'static AnyClass> = OnceLock::new();

/// The replacement `inputAssistantItem`: the original's item with both bar
/// button group lists emptied, so the keyboard shows no shortcut strip.
/// UIKit reads the item again and again while the keyboard is up, and a
/// setter may tell the assistant view to rebuild, so a list is only set
/// when it is not empty already (the next read finds nothing to do).
unsafe extern "C-unwind" fn bare_assistant_item(this: *mut AnyObject, cmd: Sel) -> *mut AnyObject {
    let Some(original) = ORIGINAL_ASSISTANT_ITEM.get() else {
        return std::ptr::null_mut();
    };
    // SAFETY: `original` is the method this one replaced, called with the
    // arguments UIKit gave us.
    let item = unsafe { original(this, cmd) };
    if !item.is_null()
        && let Some(array) = NS_ARRAY.get()
    {
        // SAFETY: `item` is a UITextInputAssistantItem (the method's return
        // type): its group lists are NSArrays or nil (nil counts as empty and
        // is never messaged: objc2's debug checks panic on a nil receiver),
        // and it has these setters taking an NSArray; `+[NSArray array]` is
        // an empty, autoreleased one.
        unsafe {
            let leading: *mut AnyObject = msg_send![item, leadingBarButtonGroups];
            if !leading.is_null() {
                let count: usize = msg_send![leading, count];
                if count > 0 {
                    let empty: *mut AnyObject = msg_send![*array, array];
                    let () = msg_send![item, setLeadingBarButtonGroups: empty];
                }
            }
            let trailing: *mut AnyObject = msg_send![item, trailingBarButtonGroups];
            if !trailing.is_null() {
                let count: usize = msg_send![trailing, count];
                if count > 0 {
                    let empty: *mut AnyObject = msg_send![*array, array];
                    let () = msg_send![item, setTrailingBarButtonGroups: empty];
                }
            }
        }
    }
    item
}

/// Removes the iPad keyboard's shortcut strip for every webview in the app.
/// Call once with `hide_form_accessory_bar`. If WebKit renames the class or
/// UIKit the method, this logs and leaves the strip in place.
pub fn hide_shortcut_strip() {
    let Some(class) = AnyClass::get(CONTENT_VIEW) else {
        tracing::warn!("WKContentView not found; the keyboard's shortcut strip stays");
        return;
    };
    let Some(array) = AnyClass::get(c"NSArray") else {
        tracing::warn!("NSArray not found; the keyboard's shortcut strip stays");
        return;
    };
    let cls = std::ptr::from_ref(class);
    // SAFETY: looking up a method and reading its implementation.
    let original = unsafe {
        let method = class_getInstanceMethod(cls, sel!(inputAssistantItem));
        if method.is_null() {
            None
        } else {
            method_getImplementation(method)
        }
    };
    let Some(original) = original else {
        tracing::warn!("inputAssistantItem not found; the keyboard's shortcut strip stays");
        return;
    };
    // SAFETY: the method has the `AssistantItemFn` signature; an `Imp` is
    // that function with its type erased.
    let original = unsafe { std::mem::transmute::<Imp, AssistantItemFn>(original) };
    let _ = NS_ARRAY.set(array);
    if ORIGINAL_ASSISTANT_ITEM.set(original).is_err() {
        // Already done: wrapping twice would call the wrapper from itself.
        return;
    }
    // SAFETY: `bare_assistant_item` matches the method's signature (object
    // return, self + _cmd), which `RETURNS_OBJECT` says; as above for the
    // `Imp` conversion.
    unsafe {
        let imp = std::mem::transmute::<AssistantItemFn, Imp>(bare_assistant_item);
        class_replaceMethod(
            cls.cast_mut(),
            sel!(inputAssistantItem),
            imp,
            RETURNS_OBJECT.as_ptr().cast::<c_char>(),
        );
    }
}

/// Makes the webview the first responder, so a hardware keyboard's keys go
/// to the page (`-[WKWebView becomeFirstResponder]` hands it to its content
/// view). No software keyboard comes up: that needs a focused text field.
/// Call when the page has loaded and when the app comes to the front.
pub fn claim_hardware_keys<R: tauri::Runtime>(webview: &tauri::Webview<R>) {
    let result = webview.with_webview(|platform| {
        let view = platform.inner().cast::<AnyObject>();
        if view.is_null() {
            return;
        }
        // SAFETY: on iOS `inner` is the WKWebView (a UIResponder), and
        // `with_webview` runs this on the main thread, where UIKit wants it.
        let _: bool = unsafe { msg_send![view, becomeFirstResponder] };
    });
    if let Err(e) = result {
        tracing::warn!("could not give the webview the keyboard focus: {e}");
    }
}
