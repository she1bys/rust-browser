//! Modern popup menus for the main options button (⋮) and tab right-click menu.
//!
//! Uses a small owned borderless `tao` window with a `WebView` so it can drop
//! down over page content while matching the browser's exact typography, icons,
//! rounded corners, and light/dark theme.

use std::time::{Duration, Instant};

use serde::Serialize;
use tao::{
    dpi::{LogicalSize, PhysicalPosition, PhysicalSize},
    event_loop::{EventLoopProxy, EventLoopWindowTarget},
    platform::windows::{WindowBuilderExtWindows, WindowExtWindows},
    window::{Theme as WindowTheme, Window, WindowBuilder, WindowId},
};
use webview2_com::{FocusChangedEventHandler, Microsoft::Web::WebView2::Win32::ICoreWebView2Environment};
use windows_sys::Win32::{
    Foundation::HWND,
    UI::{
        Input::KeyboardAndMouse::GetFocus,
        WindowsAndMessaging::{GetForegroundWindow, IsChild, ShowWindow, SW_HIDE, SW_SHOWNOACTIVATE},
    },
};
use wry::{
    MemoryUsageLevel, NewWindowResponse, PageLoadEvent, WebView, WebViewBuilder, WebViewBuilderExtWindows,
    WebViewExtWindows,
};

use crate::ipc::{Command, TabCommand, TabId, UiMsg, UserEvent};

const MENU_HTML: &str = include_str!("../ui/menu.html");
const MAIN_WIDTH: f64 = 280.0;
const TAB_WIDTH: f64 = 228.0;
/// Size of the tab hover card. Must match `#card` in `ui/menu.html`.
pub const CARD_WIDTH: f64 = 290.0;
pub const CARD_HEIGHT: f64 = 124.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuKind {
    Main,
    Tab(TabId),
    /// The memory manager, under the memory button.
    Memory,
}

/// Width of the memory manager. Its height depends on the number of tabs.
pub const MEMORY_WIDTH: f64 = 372.0;

/// Height of the memory manager for `tabs` tabs: the summary, the memory
/// saver switch and the button at the bottom, plus up to eight tab rows
/// (more scroll). Matches `#mem` in `ui/menu.html`.
pub fn memory_panel_height(tabs: usize) -> f64 {
    const FIXED: f64 = 226.0;
    const ROW: f64 = 46.0;
    FIXED + ROW * tabs.clamp(1, 8) as f64
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum MenuEntry {
    Item {
        id: MenuItemId,
        label: String,
        #[serde(skip_serializing_if = "str::is_empty")]
        shortcut: &'static str,
        icon: &'static str,
        enabled: bool,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        danger: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        checked: Option<bool>,
    },
    Zoom {
        percent: u16,
        #[serde(rename = "canIn")]
        can_in: bool,
        #[serde(rename = "canOut")]
        can_out: bool,
        #[serde(rename = "hasView")]
        has_view: bool,
        fullscreen: bool,
    },
    Sep,
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum MenuItemId {
    Main(Command),
    Tab(TabCommand),
}

impl MenuEntry {
    pub fn item(
        id: Command,
        label: impl Into<String>,
        shortcut: &'static str,
        icon: &'static str,
        enabled: bool,
    ) -> Self {
        Self::Item {
            id: MenuItemId::Main(id),
            label: label.into(),
            shortcut,
            icon,
            enabled,
            danger: false,
            checked: None,
        }
    }

    pub fn toggle_item(
        id: Command,
        label: impl Into<String>,
        shortcut: &'static str,
        checked: bool,
        enabled: bool,
    ) -> Self {
        Self::Item {
            id: MenuItemId::Main(id),
            label: label.into(),
            shortcut,
            icon: if checked { "check" } else { "" },
            enabled,
            danger: false,
            checked: Some(checked),
        }
    }

    pub fn danger(
        id: Command,
        label: impl Into<String>,
        shortcut: &'static str,
        icon: &'static str,
        enabled: bool,
    ) -> Self {
        Self::Item {
            id: MenuItemId::Main(id),
            label: label.into(),
            shortcut,
            icon,
            enabled,
            danger: true,
            checked: None,
        }
    }

    pub fn tab_item(
        id: TabCommand,
        label: impl Into<String>,
        shortcut: &'static str,
        icon: &'static str,
        enabled: bool,
    ) -> Self {
        Self::Item {
            id: MenuItemId::Tab(id),
            label: label.into(),
            shortcut,
            icon,
            enabled,
            danger: false,
            checked: None,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MenuPayload<'a> {
    tab_id: Option<TabId>,
    entries: &'a [MenuEntry],
}

/// After this long without being shown, the popup's web view is closed to
/// free its engine process; it's created again the next time it's needed.
const IDLE_RELEASE: Duration = Duration::from_secs(120);

/// How the window appears once its content is ready.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Present {
    /// As a menu or the memory manager: takes keyboard focus.
    Menu(MenuKind),
    /// As a tab's hover card: never takes focus.
    Card,
}

// `webview` must drop before `window`.
//
// The same small window shows either a menu (it takes keyboard focus) or a
// tab's hover card (it never does). Only one shows at a time; a menu wins.
//
// The web view, and with it a whole engine (renderer) process, only exists
// while it's in use: it's created the first time something is shown and closed
// again after `IDLE_RELEASE`. Until the new web view has loaded, what to show
// waits in `pending` (see `on_ready`).
pub struct PopupMenu {
    webview: Option<WebView>,
    window: Window,
    /// `webview` has loaded `menu.html` and can render.
    ready: bool,
    /// Script to run and how to show the window once the web view is ready.
    pending: Option<(String, Present)>,
    environment: ICoreWebView2Environment,
    webview_theme: wry::Theme,
    proxy: EventLoopProxy<UserEvent>,
    last_used: Instant,
    visible: bool,
    kind: Option<MenuKind>,
    closed_at: Instant,
    /// The tab whose hover card is showing.
    card: Option<TabId>,
}

impl PopupMenu {
    /// Creates the (hidden) window. Its web view is only created when first needed.
    pub fn new(
        target: &EventLoopWindowTarget<UserEvent>,
        owner: &Window,
        environment: ICoreWebView2Environment,
        window_theme: Option<WindowTheme>,
        webview_theme: wry::Theme,
        proxy: EventLoopProxy<UserEvent>,
    ) -> wry::Result<Self> {
        let window = WindowBuilder::new()
            .with_decorations(false)
            .with_undecorated_shadow(true)
            .with_resizable(false)
            .with_visible(false)
            .with_skip_taskbar(true)
            .with_owner_window(owner.hwnd())
            .with_theme(window_theme)
            .with_inner_size(LogicalSize::new(MAIN_WIDTH, 360.0))
            .build(target)
            .map_err(|_| wry::Error::InitScriptError)?;

        Ok(Self {
            webview: None,
            window,
            ready: false,
            pending: None,
            environment,
            webview_theme,
            proxy,
            last_used: Instant::now(),
            visible: false,
            kind: None,
            closed_at: Instant::now() - Duration::from_secs(10),
            card: None,
        })
    }

    /// Creates the web view if it doesn't exist. It loads in the background
    /// and reports `UserEvent::MenuReady` when done.
    fn ensure_webview(&mut self) -> bool {
        if self.webview.is_some() {
            return true;
        }
        let proxy = self.proxy.clone();
        let focus_proxy = self.proxy.clone();
        let ready_proxy = self.proxy.clone();
        let built = WebViewBuilder::new()
            .with_environment(self.environment.clone())
            .with_html(MENU_HTML)
            .with_theme(self.webview_theme)
            .with_ipc_handler(move |request| {
                if let Ok(msg) = serde_json::from_str::<UiMsg>(request.body()) {
                    let _ = proxy.send_event(UserEvent::Ui(msg));
                }
            })
            .with_on_page_load_handler(move |event, _| {
                if matches!(event, PageLoadEvent::Finished) {
                    let _ = ready_proxy.send_event(UserEvent::MenuReady);
                }
            })
            .with_new_window_req_handler(|_, _| NewWindowResponse::Deny)
            .with_drag_drop_handler(|_| true)
            .with_browser_accelerator_keys(false)
            .with_devtools(cfg!(debug_assertions))
            .build(&self.window);
        let webview = match built {
            Ok(webview) => webview,
            Err(e) => {
                eprintln!("create popup web view: {e}");
                return false;
            }
        };
        let _ = crate::webview_hooks::configure_ui_webview(&webview);
        let mut token = 0i64;
        // SAFETY: live WebView2 controller owned by the UI thread; `token` is a valid out-pointer.
        unsafe {
            let _ = webview.controller().add_LostFocus(
                &FocusChangedEventHandler::create(Box::new(move |_, _| {
                    let _ = focus_proxy.send_event(UserEvent::MenuLostFocus);
                    Ok(())
                })),
                &mut token,
            );
        }
        self.webview = Some(webview);
        self.ready = false;
        true
    }

    /// The web view finished loading: show whatever was waiting for it.
    pub fn on_ready(&mut self) {
        if self.webview.is_none() {
            return;
        }
        self.ready = true;
        let Some((script, present)) = self.pending.take() else {
            // Nothing to show any more (it was closed while loading).
            if let Some(webview) = &self.webview {
                let _ = webview.set_memory_usage_level(MemoryUsageLevel::Low);
            }
            return;
        };
        match present {
            Present::Menu(kind) if self.visible && self.kind == Some(kind) => {
                self.eval(&script);
                self.reveal_menu();
            }
            Present::Card if self.card.is_some() && !self.visible => {
                self.eval(&script);
                self.reveal_card();
            }
            _ => {}
        }
    }

    /// Runs `script` now if the web view is ready; otherwise keeps it (and how
    /// to show the window) for `on_ready`. Returns true if it ran now.
    fn run_or_queue(&mut self, script: String, present: Present) -> bool {
        self.last_used = Instant::now();
        if self.ready && self.webview.is_some() {
            self.eval(&script);
            return true;
        }
        if self.ensure_webview() {
            self.pending = Some((script, present));
        }
        false
    }

    fn eval(&self, script: &str) {
        if let Some(webview) = &self.webview {
            let _ = webview.evaluate_script(script);
        }
    }

    /// Closes the web view (freeing its engine process) if it hasn't been
    /// used for a while. Called every few seconds.
    pub fn release_if_idle(&mut self) {
        let idle = !self.visible && self.card.is_none() && self.pending.is_none();
        if self.webview.is_some() && idle && self.last_used.elapsed() >= IDLE_RELEASE {
            self.webview = None;
            self.ready = false;
        }
    }

    /// The web view exists (for tests and diagnostics).
    #[allow(dead_code)]
    pub fn has_webview(&self) -> bool {
        self.webview.is_some()
    }

    fn reveal_card(&self) {
        if let Some(webview) = &self.webview {
            let _ = webview.set_memory_usage_level(MemoryUsageLevel::Normal);
            let _ = webview.set_visible(true);
        }
        // SAFETY: a valid window handle owned by this thread.
        unsafe { ShowWindow(self.window.hwnd() as HWND, SW_SHOWNOACTIVATE) };
    }

    /// Shows tab `id`'s hover card below screen point `sx`, `sy`, without
    /// taking keyboard focus. Does nothing while a menu is open.
    pub fn show_card(&mut self, owner: &Window, id: TabId, sx: i32, sy: i32, card_json: &str) {
        if self.visible {
            return;
        }
        let scale = owner.scale_factor();
        let w = (CARD_WIDTH * scale).round() as i32;
        let h = (CARD_HEIGHT * scale).round() as i32;
        let (px, py) = clamp_position(owner, sx, sy, w, h, false);
        let moving = self.card != Some(id);
        if moving || self.card.is_none() {
            self.window.set_outer_position(PhysicalPosition::new(px, py));
            self.window.set_inner_size(PhysicalSize::new(w as u32, h as u32));
        }
        let was_showing = self.card.is_some();
        self.card = Some(id);
        let script = format!("window.menu && window.menu.renderCard({card_json})");
        if self.run_or_queue(script, Present::Card) && !was_showing {
            self.reveal_card();
        }
    }

    /// Refreshes the card's contents (e.g. the live memory figure).
    pub fn update_card(&mut self, card_json: &str) {
        if self.card.is_some() && !self.visible {
            let script = format!("window.menu && window.menu.renderCard({card_json})");
            if self.ready {
                self.last_used = Instant::now();
                self.eval(&script);
            } else if let Some((pending, Present::Card)) = &mut self.pending {
                *pending = script;
            }
        }
    }

    pub fn card_tab(&self) -> Option<TabId> {
        self.card
    }

    pub fn hide_card(&mut self) {
        if self.card.take().is_some() && !self.visible {
            if matches!(self.pending, Some((_, Present::Card))) {
                self.pending = None;
            }
            // SAFETY: a valid window handle owned by this thread.
            unsafe { ShowWindow(self.window.hwnd() as HWND, SW_HIDE) };
            if let Some(webview) = &self.webview {
                let _ = webview.set_memory_usage_level(MemoryUsageLevel::Low);
            }
        }
    }

    pub fn is_window(&self, id: WindowId) -> bool {
        self.window.id() == id
    }

    /// True when the popup window or its hosted WebView2 child owns Windows focus.
    pub fn owns_focus(&self) -> bool {
        let hwnd = self.window.hwnd() as HWND;
        // SAFETY: Win32 HWND query APIs accept any window handle without mutating memory.
        unsafe {
            if GetForegroundWindow() == hwnd {
                return true;
            }
            let focused = GetFocus();
            !focused.is_null() && (focused == hwnd || IsChild(hwnd, focused) != 0)
        }
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn kind(&self) -> Option<MenuKind> {
        self.kind
    }

    /// True if the menu was just dismissed by clicking the button that toggles it.
    pub fn recently_closed(&self) -> bool {
        self.closed_at.elapsed() < Duration::from_millis(220)
    }

    /// The window's own theme, and the one a newly created web view starts
    /// with (the engine's theme is shared with the rest of the browser).
    pub fn set_theme(&mut self, theme: Option<WindowTheme>, webview_theme: wry::Theme) {
        self.window.set_theme(theme);
        self.webview_theme = webview_theme;
    }

    /// A menu replaces any hover card in the same window.
    fn drop_card_for_menu(&mut self) {
        if self.card.take().is_some() {
            if matches!(self.pending, Some((_, Present::Card))) {
                self.pending = None;
            }
            // SAFETY: a valid window handle owned by this thread.
            unsafe { ShowWindow(self.window.hwnd() as HWND, SW_HIDE) };
        }
    }

    pub fn show(&mut self, owner: &Window, kind: MenuKind, sx: i32, sy: i32, align_right: bool, entries: &[MenuEntry]) {
        self.drop_card_for_menu();
        let width = match kind {
            MenuKind::Main => MAIN_WIDTH,
            MenuKind::Tab(_) => TAB_WIDTH,
            MenuKind::Memory => MEMORY_WIDTH,
        };
        let height = measure_height(entries);
        let scale = owner.scale_factor();
        let phys_w = (width * scale).round().max(1.0) as i32;
        let phys_h = (height * scale).round().max(1.0) as i32;
        let (px, py) = clamp_position(owner, sx, sy, phys_w, phys_h, align_right);

        // Move onto the owner window's monitor before resizing so cross-DPI
        // scale transitions don't apply the previous monitor's scale factor.
        self.window.set_outer_position(PhysicalPosition::new(px, py));
        self.window.set_inner_size(PhysicalSize::new(phys_w as u32, phys_h as u32));

        let tab_id = match kind {
            MenuKind::Main | MenuKind::Memory => None,
            MenuKind::Tab(id) => Some(id),
        };
        let Ok(json) = serde_json::to_string(&MenuPayload { tab_id, entries }) else { return };
        self.present(kind, format!("window.menu && window.menu.render({json})"));
    }

    /// Opens the memory manager below screen point `sx`, `sy` (right-aligned)
    /// with room for `tabs` tab rows. `panel_json` is its first contents.
    pub fn show_memory(&mut self, owner: &Window, sx: i32, sy: i32, tabs: usize, panel_json: &str) {
        self.drop_card_for_menu();
        let scale = owner.scale_factor();
        let w = (MEMORY_WIDTH * scale).round() as i32;
        let h = (memory_panel_height(tabs) * scale).round() as i32;
        let (px, py) = clamp_position(owner, sx, sy, w, h, true);
        self.window.set_outer_position(PhysicalPosition::new(px, py));
        self.window.set_inner_size(PhysicalSize::new(w as u32, h as u32));
        self.present(MenuKind::Memory, format!("window.menu && window.menu.renderMemory({panel_json})"));
    }

    /// Refreshes the memory manager's figures while it is open.
    pub fn update_memory(&self, panel_json: &str) {
        if self.visible && self.kind == Some(MenuKind::Memory) && self.ready {
            self.eval(&format!("window.menu && window.menu.renderMemory({panel_json})"));
        }
    }

    /// Renders `script` and shows the window with keyboard focus, as a menu
    /// (once the web view is ready, if it was just created).
    fn present(&mut self, kind: MenuKind, script: String) {
        self.visible = true;
        self.kind = Some(kind);
        if self.run_or_queue(script, Present::Menu(kind)) {
            self.reveal_menu();
        }
    }

    fn reveal_menu(&self) {
        let Some(webview) = &self.webview else { return };
        let _ = webview.set_memory_usage_level(MemoryUsageLevel::Normal);
        let _ = webview.set_visible(true);
        self.window.set_visible(true);
        self.window.set_focus();
        let _ = webview.focus();
    }

    pub fn update_entries(&self, entries: &[MenuEntry]) {
        if !self.visible || !self.ready {
            return;
        }
        let tab_id = match self.kind {
            Some(MenuKind::Tab(id)) => Some(id),
            _ => None,
        };
        if let Ok(json) = serde_json::to_string(&MenuPayload { tab_id, entries }) {
            self.eval(&format!("window.menu && window.menu.render({json})"));
        }
    }

    /// Closes the menu, and any hover card.
    pub fn hide(&mut self) {
        self.hide_card();
        if self.visible {
            if matches!(self.pending, Some((_, Present::Menu(_)))) {
                self.pending = None;
            }
            self.window.set_visible(false);
            // Stops the memory manager's once-a-second refresh, if it was open.
            self.eval("window.menu && window.menu.onHide && window.menu.onHide()");
            self.visible = false;
            self.kind = None;
            self.closed_at = Instant::now();
            self.last_used = Instant::now();
            if let Some(webview) = &self.webview {
                let _ = webview.set_memory_usage_level(MemoryUsageLevel::Low);
            }
        }
    }
}

/// Computes the exact logical height of `entries` matching `ui/menu.html` CSS.
pub fn measure_height(entries: &[MenuEntry]) -> f64 {
    let mut h = 12.0; // 6px top + 6px bottom padding on #menu
    for (i, entry) in entries.iter().enumerate() {
        if i > 0 {
            h += 1.0; // flex gap
        }
        h += match entry {
            MenuEntry::Item { .. } => 32.0,
            MenuEntry::Zoom { .. } => 36.0,
            MenuEntry::Sep => 9.0, // 1px line + 4px top/bottom margin
        };
    }
    h
}

fn clamp_position(owner: &Window, sx: i32, sy: i32, w: i32, h: i32, align_right: bool) -> (i32, i32) {
    if let Some(mon) = owner.current_monitor() {
        let pos = mon.position();
        let size = mon.size();
        clamp_to_area((sx, sy), (w, h), align_right, (pos.x, pos.y, size.width as i32, size.height as i32))
    } else {
        (if align_right { sx - w } else { sx }, sy + 4)
    }
}

fn clamp_to_area(anchor: (i32, i32), size: (i32, i32), align_right: bool, area: (i32, i32, i32, i32)) -> (i32, i32) {
    let (sx, sy) = anchor;
    let (w, h) = size;
    let (area_x, area_y, area_w, area_h) = area;
    let mut x = if align_right { sx - w } else { sx };
    let mut y = sy + 4;
    let min_x = area_x + 6;
    let max_x = area_x + area_w - w - 6;
    let min_y = area_y + 6;
    let max_y = area_y + area_h - h - 6;
    x = x.clamp(min_x, max_x.max(min_x));
    if y > max_y && sy - h - 4 >= min_y {
        y = sy - h - 4;
    } else {
        y = y.clamp(min_y, max_y.max(min_y));
    }
    (x, y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_panel_grows_with_tabs_up_to_eight_rows() {
        assert_eq!(memory_panel_height(0), memory_panel_height(1));
        assert!(memory_panel_height(3) > memory_panel_height(2));
        assert_eq!(memory_panel_height(8), memory_panel_height(40), "more than eight rows scroll");
    }

    #[test]
    fn measures_menu_height_deterministically() {
        let entries = [
            MenuEntry::item(Command::NewTab, "New tab", "Ctrl+T", "newTab", true),
            MenuEntry::Sep,
            MenuEntry::Zoom { percent: 100, can_in: true, can_out: true, has_view: true, fullscreen: false },
        ];
        // 12 padding + 32 + 9 + 36 + 2*1 gaps = 91
        assert_eq!(measure_height(&entries), 91.0);
    }

    #[test]
    fn clamp_to_area_keeps_menu_on_monitor_and_flips_up_near_bottom() {
        // Right-aligned near right/bottom edge on a secondary monitor at (-1920, 0).
        let (x, y) = clamp_to_area((-1910, 1060), (280, 300), true, (-1920, 0, 1920, 1080));
        assert_eq!(x, -1914);
        assert_eq!(y, 756);
    }
}
