//! The blur behind a translucent window.
//!
//! gpui can already do this — [`gpui::WindowBackgroundAppearance::Blurred`]
//! hangs an `NSVisualEffectView` under the content view — but the material it
//! asks for is `NSVisualEffectMaterialSelection`, and on macOS 26 that material
//! frosts nothing at all. The window comes out plainly see-through: the desktop
//! behind it is not softened, it is simply *there*, sharp enough to read a line
//! of someone else's terminal through the sidebar. The sidebar material still
//! frosts the way it always did.
//!
//! So we take only the transparency from gpui and hang our own effect view,
//! with the material Finder uses, in the same place.

// `objc`'s `msg_send!` expands to a `cfg(feature = "cargo-clippy")` check that
// this crate does not have, once per call.
#[allow(unexpected_cfgs)]
#[cfg(target_os = "macos")]
mod imp {
    use objc::runtime::{Object, BOOL, NO};
    use objc::{class, msg_send, sel, sel_impl};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    type Id = *mut Object;

    // AppKit constants. Spelled as numbers because nothing in the tree binds
    // AppKit and one enum variant each is not worth a crate.
    const MATERIAL_SIDEBAR: i64 = 7;
    const BLENDING_BEHIND_WINDOW: i64 = 0;
    const STATE_ACTIVE: i64 = 1;
    /// `NSViewWidthSizable | NSViewHeightSizable`.
    const RESIZE_WITH_SUPERVIEW: u64 = 2 | 16;
    /// `NSWindowBelow`.
    const ORDER_BELOW: i64 = -1;

    #[repr(C)]
    #[derive(Copy, Clone)]
    struct NSRect {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    }

    /// Put the blur behind `window`, or take it away again.
    ///
    /// Idempotent: the view is found by its class rather than remembered, so a
    /// second call with the same answer does nothing and a window that never
    /// had one is not disturbed by being told to remove it.
    ///
    /// `dark` is the *theme's* appearance, not the system's. The effect view
    /// is a real AppKit view and frosts in whichever appearance it inherits,
    /// so a light theme on a dark Mac would tint a dark frost and come out as
    /// mud with light text on it.
    pub fn apply(window: &gpui::Window, on: bool, dark: bool) {
        let handle = match HasWindowHandle::window_handle(window) {
            Ok(handle) => handle,
            Err(_) => return,
        };
        let view: Id = match handle.as_raw() {
            RawWindowHandle::AppKit(appkit) => appkit.ns_view.as_ptr().cast(),
            _ => return,
        };

        unsafe {
            // gpui draws into a view of its own inside the window's content
            // view, which is the one the blur has to sit under.
            let content: Id = msg_send![view, superview];
            if content.is_null() {
                return;
            }

            let existing = find_effect_view(content);
            match (on, existing) {
                (true, Some(effect)) => set_appearance(effect, dark),
                (true, None) => {
                    let bounds: NSRect = msg_send![content, bounds];
                    let effect: Id = msg_send![class!(NSVisualEffectView), alloc];
                    let effect: Id = msg_send![effect, initWithFrame: bounds];
                    let _: () = msg_send![effect, setMaterial: MATERIAL_SIDEBAR];
                    let _: () = msg_send![effect, setBlendingMode: BLENDING_BEHIND_WINDOW];
                    // Active regardless of whether the window is key: a sidebar
                    // that goes flat when you click another app is a sidebar
                    // that redraws itself every time you glance away.
                    let _: () = msg_send![effect, setState: STATE_ACTIVE];
                    let _: () = msg_send![effect, setAutoresizingMask: RESIZE_WITH_SUPERVIEW];
                    set_appearance(effect, dark);
                    let _: () = msg_send![content, addSubview: effect
                                                   positioned: ORDER_BELOW
                                                   relativeTo: std::ptr::null_mut::<Object>()];
                    let _: () = msg_send![effect, release];
                }
                (false, Some(effect)) => {
                    let _: () = msg_send![effect, removeFromSuperview];
                }
                (false, None) => {}
            }
        }
    }

    /// Frost light or frost dark, whatever the rest of the Mac is doing.
    unsafe fn set_appearance(effect: Id, dark: bool) {
        // NUL-terminated by hand: these are C strings, and a Rust literal is
        // not one.
        let name: &[u8] = match dark {
            true => b"NSAppearanceNameDarkAqua\0",
            false => b"NSAppearanceNameAqua\0",
        };
        let name = name.as_ptr().cast::<std::os::raw::c_char>();
        let string: Id = msg_send![class!(NSString), stringWithUTF8String: name];
        if string.is_null() {
            return;
        }
        let appearance: Id = msg_send![class!(NSAppearance), appearanceNamed: string];
        let _: () = msg_send![effect, setAppearance: appearance];
    }

    unsafe fn find_effect_view(content: Id) -> Option<Id> {
        let subviews: Id = msg_send![content, subviews];
        if subviews.is_null() {
            return None;
        }
        let count: u64 = msg_send![subviews, count];
        let class = class!(NSVisualEffectView);
        for i in 0..count {
            let subview: Id = msg_send![subviews, objectAtIndex: i];
            let hit: BOOL = msg_send![subview, isKindOfClass: class];
            if hit != NO {
                return Some(subview);
            }
        }
        None
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    pub fn apply(_window: &gpui::Window, _on: bool, _dark: bool) {}
}

pub use imp::apply;
