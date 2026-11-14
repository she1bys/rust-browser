//! Browser keyboard shortcuts pressed while a web page has focus.
//!
//! Key presses inside a page never reach the browser UI, so they are caught
//! here, at the WebView2 level, before the page or WebView2's own shortcuts
//! see them. Doing it natively (rather than with a script injected into each
//! page) means it also works on blank and error pages, a page can't block or
//! fake a shortcut, and pages need no message channel to the browser at all.

use tao::event_loop::EventLoopProxy;
use webview2_com::{
    AcceleratorKeyPressedEventHandler,
    Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_KEY_EVENT_KIND, COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN, COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN,
        COREWEBVIEW2_PHYSICAL_KEY_STATUS,
    },
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, VK_CONTROL, VK_F11, VK_HOME, VK_MENU, VK_NUMPAD0, VK_OEM_COMMA, VK_SHIFT, VK_TAB,
};
use wry::{WebView, WebViewExtWindows};

use crate::ipc::{Shortcut, TabId, UserEvent};

/// Maps a key press to a browser shortcut. `key` is a Windows virtual-key
/// code (letters are their uppercase ASCII value).
pub fn shortcut_for(key: u32, ctrl: bool, shift: bool, alt: bool) -> Option<Shortcut> {
    const TAB: u32 = VK_TAB as u32;
    const COMMA: u32 = VK_OEM_COMMA as u32;
    const HOME: u32 = VK_HOME as u32;
    const NUMPAD0: u32 = VK_NUMPAD0 as u32;
    const F11: u32 = VK_F11 as u32;
    match (ctrl, shift, alt, key) {
        (true, false, false, k) if k == 'T' as u32 => Some(Shortcut::NewTab),
        (true, true, false, k) if k == 'N' as u32 => Some(Shortcut::NewPrivateTab),
        (true, true, false, k) if k == 'O' as u32 => Some(Shortcut::Bookmarks),
        (true, true, false, k) if k == 'B' as u32 => Some(Shortcut::ToggleBookmarksBar),
        (true, false, false, k) if k == 'S' as u32 => Some(Shortcut::SavePage),
        (false, false, false, F11) => Some(Shortcut::Fullscreen),
        (false, false, true, k) if k == 'F' as u32 || k == 'E' as u32 => Some(Shortcut::Menu),
        (true, true, false, k) if k == 'T' as u32 => Some(Shortcut::ReopenTab),
        (true, false, false, k) if k == 'W' as u32 => Some(Shortcut::CloseTab),
        (true, false, false, k) if k == 'L' as u32 => Some(Shortcut::FocusAddress),
        (false, false, true, k) if k == 'D' as u32 => Some(Shortcut::FocusAddress),
        // Without this, WebView2 would open its own (empty) downloads popup.
        (true, false, false, k) if k == 'J' as u32 => Some(Shortcut::Downloads),
        (true, false, false, k) if k == 'H' as u32 => Some(Shortcut::History),
        (true, false, false, k) if k == 'D' as u32 => Some(Shortcut::Bookmark),
        (true, false, false, COMMA) => Some(Shortcut::Settings),
        (false, false, true, HOME) => Some(Shortcut::Home),
        (true, false, false, k) if k == '0' as u32 || k == NUMPAD0 => Some(Shortcut::ResetZoom),
        (true, false, false, k) if k >= '1' as u32 && k <= '8' as u32 => {
            Some(Shortcut::SelectTab((k - '1' as u32) as usize))
        }
        (true, false, false, k) if k == '9' as u32 => Some(Shortcut::LastTab),
        (true, false, false, TAB) => Some(Shortcut::NextTab),
        (true, true, false, TAB) => Some(Shortcut::PrevTab),
        _ => None,
    }
}

/// Starts catching browser shortcuts in `webview` (the page of tab `tab`).
pub fn attach(webview: &WebView, tab: TabId, proxy: EventLoopProxy<UserEvent>) -> windows::core::Result<()> {
    let handler = AcceleratorKeyPressedEventHandler::create(Box::new(move |_, args| {
        let Some(args) = args else { return Ok(()) };
        // SAFETY: plain COM calls on the live event object WebView2 handed us,
        // on the thread that owns it; out-pointers are valid locals.
        unsafe {
            let mut kind = COREWEBVIEW2_KEY_EVENT_KIND::default();
            args.KeyEventKind(&mut kind)?;
            if kind != COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN && kind != COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN {
                return Ok(());
            }

            let mut key = 0u32;
            args.VirtualKey(&mut key)?;
            let held = |vk: u16| GetKeyState(i32::from(vk)) < 0;
            let Some(shortcut) = shortcut_for(key, held(VK_CONTROL), held(VK_SHIFT), held(VK_MENU)) else {
                return Ok(());
            };

            // Ours: keep it away from the page and from WebView2.
            args.SetHandled(true)?;

            // Holding the keys down repeats the event. That is useful for
            // flipping through tabs, but must not open or close a pile of them.
            let mut status = COREWEBVIEW2_PHYSICAL_KEY_STATUS::default();
            args.PhysicalKeyStatus(&mut status)?;
            let repeat = status.WasKeyDown.as_bool();
            if !repeat || matches!(shortcut, Shortcut::NextTab | Shortcut::PrevTab) {
                let _ = proxy.send_event(UserEvent::Shortcut(tab, shortcut));
            }
        }
        Ok(())
    }));

    let mut token = 0i64;
    // SAFETY: the controller is live and `token` is a valid out-pointer.
    unsafe { webview.controller().add_AcceleratorKeyPressed(&handler, &mut token) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TAB: u32 = VK_TAB as u32;

    #[test]
    fn maps_browser_shortcuts() {
        assert_eq!(shortcut_for('T' as u32, true, false, false), Some(Shortcut::NewTab));
        assert_eq!(shortcut_for('T' as u32, true, true, false), Some(Shortcut::ReopenTab));
        assert_eq!(shortcut_for('B' as u32, true, true, false), Some(Shortcut::ToggleBookmarksBar));
        assert_eq!(shortcut_for('W' as u32, true, false, false), Some(Shortcut::CloseTab));
        assert_eq!(shortcut_for('L' as u32, true, false, false), Some(Shortcut::FocusAddress));
        assert_eq!(shortcut_for('D' as u32, false, false, true), Some(Shortcut::FocusAddress));
        assert_eq!(shortcut_for('J' as u32, true, false, false), Some(Shortcut::Downloads));
        assert_eq!(shortcut_for('H' as u32, true, false, false), Some(Shortcut::History));
        assert_eq!(shortcut_for('D' as u32, true, false, false), Some(Shortcut::Bookmark));
        assert_eq!(shortcut_for(VK_OEM_COMMA as u32, true, false, false), Some(Shortcut::Settings));
        assert_eq!(shortcut_for(VK_HOME as u32, false, false, true), Some(Shortcut::Home));
        assert_eq!(shortcut_for('0' as u32, true, false, false), Some(Shortcut::ResetZoom));
        assert_eq!(shortcut_for('3' as u32, true, false, false), Some(Shortcut::SelectTab(2)));
        assert_eq!(shortcut_for('9' as u32, true, false, false), Some(Shortcut::LastTab));
        assert_eq!(shortcut_for(TAB, true, false, false), Some(Shortcut::NextTab));
        assert_eq!(shortcut_for(TAB, true, true, false), Some(Shortcut::PrevTab));
        assert_eq!(shortcut_for('N' as u32, true, true, false), Some(Shortcut::NewPrivateTab));
        assert_eq!(shortcut_for('O' as u32, true, true, false), Some(Shortcut::Bookmarks));
        assert_eq!(shortcut_for('S' as u32, true, false, false), Some(Shortcut::SavePage));
        assert_eq!(shortcut_for(VK_F11 as u32, false, false, false), Some(Shortcut::Fullscreen));
        assert_eq!(shortcut_for('F' as u32, false, false, true), Some(Shortcut::Menu));
        assert_eq!(shortcut_for('E' as u32, false, false, true), Some(Shortcut::Menu));
    }

    #[test]
    fn leaves_find_print_and_devtools_to_webview2() {
        // These are handled by WebView2 itself, which does them well.
        assert_eq!(shortcut_for('F' as u32, true, false, false), None); // find
        assert_eq!(shortcut_for('P' as u32, true, false, false), None); // print
        assert_eq!(shortcut_for('N' as u32, true, false, false), None); // Ctrl+N: not a browser shortcut here
    }

    #[test]
    fn leaves_everything_else_to_the_page() {
        // Plain typing, and shortcuts that belong to the page or WebView2.
        assert_eq!(shortcut_for('T' as u32, false, false, false), None);
        assert_eq!(shortcut_for('C' as u32, true, false, false), None); // copy
        assert_eq!(shortcut_for('F' as u32, true, false, false), None); // find
        assert_eq!(shortcut_for('R' as u32, true, false, false), None); // reload
        assert_eq!(shortcut_for('W' as u32, true, true, false), None);
        assert_eq!(shortcut_for(TAB, false, false, false), None);
        assert_eq!(shortcut_for(TAB, false, false, true), None); // Alt+Tab
                                                                 // AltGr reports as Ctrl+Alt and types characters on many keyboards.
        assert_eq!(shortcut_for('L' as u32, true, false, true), None);
        assert_eq!(shortcut_for('D' as u32, true, false, true), None);
    }
}
