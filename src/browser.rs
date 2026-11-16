//! The browser itself: one window, one "chrome" web view for the UI (tab strip,
//! toolbar and panels) and one web view per tab underneath it.

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet, VecDeque},
    os::windows::process::CommandExt,
    path::PathBuf,
    process::Command,
    rc::Rc,
    time::{Duration, Instant},
};

use serde::Serialize;
use tao::{
    event_loop::{EventLoopProxy, EventLoopWindowTarget},
    platform::windows::WindowExtWindows,
    window::{Fullscreen, ResizeDirection, Window, WindowId},
};
use wry::{
    dpi::{PhysicalPosition, PhysicalSize},
    MemoryUsageLevel, NewWindowResponse, PageLoadEvent, Rect, WebContext, WebView, WebViewBuilder,
    WebViewBuilderExtWindows, WebViewExtWindows,
};

use crate::bookmarks::{Bookmark, Bookmarks};
use crate::device;
use crate::display;
use crate::download_hook::DownloadHook;
use crate::downloads::{Download, DownloadState, Downloads};
use crate::history::{History, HistoryEntry};
use crate::import;
use crate::ipc::{
    Command as BrowserCommand, DownloadAction, DownloadId, Panel, ProcessFailure, RendererInfo, Shortcut, TabCommand,
    TabId, UiMsg, UpdateEvent, UserEvent, WindowAction,
};
use crate::memory;
use crate::menu::{self, MenuEntry, MenuKind, PopupMenu};
use crate::newtab::{self, NewTabState, NEW_TAB_URL};
use crate::session::Session;
use crate::settings::{
    self, EfficiencyMode, NewTabPage, SearchEngine, Settings, SettingsPatch, Startup, Theme, ZOOM_LEVELS,
};
use crate::updater::{self, Release};
use crate::webview_hooks::PopupGestures;
use crate::{shortcut_hook, url_input, webview_hooks};
use std::sync::{Arc, RwLock};

/// Height of the browser UI in logical pixels. Must match `ui/index.html` (38 + 50 = 88).
const CHROME_HEIGHT: f64 = 88.0;
const BOOKMARKS_BAR_HEIGHT: f64 = 32.0;
const DOWNLOADS_URL: &str = "about:downloads";
const APP_NAME: &str = "Rust Browser";
const MAX_CLOSED_TABS: usize = 25;
/// How often download progress is pushed to the UI while the panel is open.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(150);
/// The first automatic update check waits this long after start-up, so it
/// never competes with opening the first pages.
const FIRST_UPDATE_CHECK: Duration = Duration::from_secs(30);
/// Then the browser checks again this often while it stays open...
const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// ...or this soon after a check that failed (offline, GitHub busy).
const UPDATE_RETRY_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

const UI_HTML: &str = include_str!("../ui/index.html");
/// Reader mode, run inside the page (see `toggle_reader`).
const READER_JS: &str = include_str!("../ui/reader.js");

/// The Windows Downloads folder (`%USERPROFILE%\Downloads`).
fn downloads_folder() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("USERPROFILE")?).join("Downloads");
    dir.is_dir().then_some(dir)
}

/// Local date and time as `2026-11-16 14.05.09` (file-name safe).
fn local_timestamp() -> String {
    use windows_sys::Win32::{Foundation::SYSTEMTIME, System::SystemInformation::GetLocalTime};
    let mut t =
        SYSTEMTIME { wYear: 0, wMonth: 0, wDayOfWeek: 0, wDay: 0, wHour: 0, wMinute: 0, wSecond: 0, wMilliseconds: 0 };
    // SAFETY: `t` is a valid, writable SYSTEMTIME.
    unsafe { GetLocalTime(&mut t) };
    format!("{:04}-{:02}-{:02} {:02}.{:02}.{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond)
}

/// "Screenshot example.com 2026-11-16 14.05.09.png", with characters that
/// aren't allowed in Windows file names left out.
fn screenshot_file_name(site: &str, timestamp: &str) -> String {
    let site: String =
        site.chars().filter(|c| !matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')).collect();
    let site = site.trim();
    if site.is_empty() {
        format!("Screenshot {timestamp}.png")
    } else {
        format!("Screenshot {site} {timestamp}.png")
    }
}

/// `folder\name`, or `folder\name (2)` etc. if that file already exists.
fn unique_path(folder: &std::path::Path, name: &str) -> PathBuf {
    let first = folder.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = name.rsplit_once('.').unwrap_or((name, ""));
    (2..)
        .map(|n| folder.join(if ext.is_empty() { format!("{stem} ({n})") } else { format!("{stem} ({n}).{ext}") }))
        .find(|p| !p.exists())
        .unwrap_or(first)
}

/// An ordinary web page (reader mode and the like make sense on it).
fn is_web_page(url: &str) -> bool {
    let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    lower.starts_with("https://") || lower.starts_with("http://")
}

struct Tab {
    id: TabId,
    /// `None` for restored background tabs that haven't been visited yet.
    webview: Option<WebView>,
    wv_visible: Cell<bool>,
    low_memory: Cell<bool>,
    applied_bounds: Cell<Option<(i32, i32, u32, u32)>>,
    title: String,
    url: String,
    favicon: String,
    loading: bool,
    can_go_back: bool,
    can_go_forward: bool,
    zoom_percent: u16,
    /// InPrivate: nothing is kept after the browser closes.
    private: bool,
    /// The page is playing sound.
    audible: bool,
    muted: bool,
    /// When the page was last hidden (it is out of sight since then).
    hidden_since: Cell<Option<Instant>>,
    /// Frozen by the memory saver (WebView2 TrySuspend); resumes when shown.
    frozen: Cell<bool>,
    /// Ads and trackers blocked on the current page (counted by the blocker
    /// in `webview_hooks::attach_ad_blocker`, reset when a new page starts).
    ads_blocked: Rc<Cell<u32>>,
    /// The blocker has reported a new count that isn't shown yet.
    ads_notified: Rc<Cell<bool>>,
}

/// One tab in the memory manager.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MemoryTabView<'a> {
    id: TabId,
    title: &'a str,
    site: String,
    /// "visible", "awake", "frozen", "sleeping" or "internal".
    state: &'static str,
    memory: Option<u64>,
    shared: usize,
    private: bool,
    audible: bool,
    can_sleep: bool,
}

/// The memory manager's contents (see `renderMemory` in `ui/menu.html`).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MemoryPanelView<'a> {
    /// Everything the browser uses: this program plus all its engine processes.
    total: u64,
    /// The part used by the tabs' pages.
    tabs_total: u64,
    processes: usize,
    saver: settings::MemorySaver,
    /// How many tabs "Free up memory" would put to sleep.
    can_free: usize,
    tabs: Vec<MemoryTabView<'a>>,
}

/// What the tab hover card shows (see `ui/menu.html`).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CardView<'a> {
    title: &'a str,
    site: String,
    private: bool,
    /// "awake", "frozen", "sleeping" or "internal" (a page of the browser itself).
    state: &'static str,
    /// Private working set of the page's processes, in bytes. `None` while
    /// the engine hasn't said yet which processes they are.
    memory: Option<u64>,
    processes: usize,
    /// How many other tabs share at least one of those processes.
    shared: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TabView<'a> {
    id: TabId,
    title: &'a str,
    url: &'a str,
    favicon: &'a str,
    loading: bool,
    can_go_back: bool,
    can_go_forward: bool,
    zoom_percent: u16,
    private: bool,
    audible: bool,
    muted: bool,
    /// Put to sleep (or never loaded): uses no memory until opened.
    sleeping: bool,
    /// Ads and trackers blocked on this page.
    ads_blocked: u32,
    /// A web page (offers reader mode).
    can_read: bool,
}

/// What the button on a notice does.
enum NoticeAction {
    OpenUrl {
        url: String,
        private: bool,
    },
    Reload(TabId),
    InstallUpdate,
    /// Show a saved file in File Explorer.
    ShowFile(PathBuf),
}

/// One open tab in tab search (see `renderTabSearch` in `ui/menu.html`).
#[derive(Serialize)]
struct TabSearchRow<'a> {
    id: TabId,
    title: &'a str,
    site: String,
    active: bool,
    private: bool,
    audible: bool,
    sleeping: bool,
}

#[derive(Serialize)]
struct TabSearchView<'a> {
    tabs: Vec<TabSearchRow<'a>>,
}

/// Where the browser is with updating itself.
#[derive(Debug, Clone, PartialEq, Eq)]
enum UpdateStatus {
    /// Not checked yet since start-up.
    Idle,
    Checking,
    UpToDate,
    /// `Browser::update_release` is newer than this copy.
    Available,
    /// Downloading the installer: percent done, if known.
    Downloading(Option<u8>),
    /// The installer is starting; the browser is about to close.
    Installing,
    Failed(String),
}

/// The update status as the UI sees it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateView<'a> {
    /// idle, checking, upToDate, available, downloading, installing or failed.
    state: &'static str,
    /// The newer version, once one was found.
    version: Option<String>,
    percent: Option<u8>,
    error: Option<&'a str>,
    /// This copy was installed with the installer and can update itself;
    /// otherwise (portable) "Update" opens the download page.
    can_install: bool,
}

#[derive(Serialize)]
struct EngineView {
    id: SearchEngine,
    name: &'static str,
}

#[derive(Serialize)]
struct DownloadView<'a> {
    id: DownloadId,
    name: String,
    url: &'a str,
    received: i64,
    total: i64,
    state: DownloadState,
    /// A finished download whose file is no longer where it was saved.
    missing: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AboutView<'a> {
    version: &'static str,
    /// Version of the WebView2 (Chromium) engine that draws the pages.
    engine: &'a str,
    /// Installed memory, in bytes, and whether this counts as a low-end PC
    /// (efficiency mode "Auto" turns on).
    memory: Option<u64>,
    low_end: bool,
}

/// A browser bookmarks and history can be imported from.
#[derive(Serialize)]
struct ImportSourceView {
    id: import::Source,
    name: &'static str,
}

/// Static configuration sent once when the UI finishes loading.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InitView<'a> {
    engines: Vec<EngineView>,
    zoom_levels: &'static [u16],
    about: AboutView<'a>,
    /// Browsers found on this PC to import from.
    import_sources: Vec<ImportSourceView>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SuggestionView<'a> {
    url: &'a str,
    title: &'a str,
    bookmarked: bool,
}

/// A short message for the user, e.g. "Browsing data cleared".
#[derive(Serialize)]
struct Notice {
    /// Goes up with every notice, so the UI shows each one exactly once.
    id: u32,
    text: String,
    error: bool,
    /// Label of the notice's button, if it has one.
    action: Option<&'static str>,
}

/// Dynamic state pushed to the UI on changes.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StateView<'a> {
    tabs: Vec<TabView<'a>>,
    active: Option<TabId>,
    /// Left and right tab IDs when split view is paired.
    split: Option<[TabId; 2]>,
    /// Drives the maximize/restore button and the top resize edge.
    maximized: bool,
    panel: Option<Panel>,
    confirm_close: bool,
    settings: &'a Settings,
    bookmarked: bool,
    status_text: &'a str,
    notice: Option<&'a Notice>,
    /// Only filled while the Downloads tab or panel is open.
    downloads: Vec<DownloadView<'a>>,
    downloads_active: usize,
    downloads_progress: Option<u8>,
    /// A download started or finished since Downloads was last viewed.
    downloads_unseen: bool,
    /// Monotonically incremented each time a new download starts.
    downloads_added_seq: u64,
    /// Name of the most recently added download.
    downloads_latest: Option<String>,
    /// Current monitor refresh rate in Hz (e.g. 60, 144, 165).
    refresh_hz: Option<u32>,
    /// Filled while Bookmarks panel or bookmarks bar is shown.
    #[serde(skip_serializing_if = "Option::is_none")]
    bookmarks: Option<&'a [Bookmark]>,
    /// Only filled while the History panel is open.
    history: &'a [HistoryEntry],
    update: UpdateView<'a>,
    /// Efficiency mode is on (by choice, or "Auto" on a low-end PC).
    efficiency: bool,
}

/// Where a newly opened tab goes in the tab strip.
#[derive(Clone, Copy)]
enum Placement {
    End,
    AfterActive,
    AfterTab(TabId),
}

// Field order matters: fields drop top to bottom. Running download handles go
// first, then the web views (including the popup menu), and the window they live in goes last.
pub struct Browser {
    download_hook: DownloadHook,
    tabs: Vec<Tab>,
    /// Closed tabs that still have a download running. They stay alive
    /// (hidden) until the download ends, so closing a tab never kills it.
    background: Vec<Tab>,
    chrome: WebView,
    menu: PopupMenu,
    web_context: WebContext,
    window: Window,

    active: Option<TabId>,
    split: Option<(TabId, TabId)>,
    next_id: TabId,
    proxy: EventLoopProxy<UserEvent>,
    /// Last maximized state sent to the UI.
    maximized: bool,
    /// Tab currently presenting an HTML5 fullscreen element (e.g. a video).
    fullscreen_tab: Option<TabId>,
    /// Full screen chosen by the user (F11 or the menu).
    window_fullscreen: bool,
    /// Set when the user confirms closing (or no downloads block exit).
    exit_requested: bool,
    /// True while showing the "active downloads will be cancelled" prompt.
    confirm_close: bool,
    /// Most recently closed tabs (address, private) for Ctrl+Shift+T.
    closed_tabs: Vec<(String, bool)>,
    /// The "Block pop-ups" setting, shared with every tab's new-window handler.
    block_popups: Rc<Cell<bool>>,
    /// The "Block ads and trackers" setting, shared with every tab's blocker.
    block_ads: Rc<Cell<bool>>,
    notice_action: Option<NoticeAction>,
    /// The engine crashed and a restart is under way.
    restarting: bool,
    started_at: Instant,
    /// This process was itself started to recover from an engine crash.
    restored_after_crash: bool,
    /// Hover link preview for the active tab.
    status_text: String,

    settings: Settings,
    settings_path: Option<PathBuf>,
    session_path: Option<PathBuf>,
    saved_session: RefCell<Option<Session>>,
    notice: Option<Notice>,
    engine_version: String,
    refresh_hz: Option<u32>,
    /// While a panel (or close prompt) is open the UI web view covers the
    /// whole window and the active tab is hidden behind it.
    panel: Option<Panel>,

    downloads: Downloads,
    downloads_path: Option<PathBuf>,
    download_tabs: HashMap<DownloadId, TabId>,
    cancelled: HashSet<DownloadId>,
    downloads_unseen: bool,
    downloads_added_seq: u64,
    last_progress_sync: Instant,
    sync_pending: Cell<bool>,
    last_title: RefCell<String>,
    bookmarks_rev: u64,
    sent_bookmarks_rev: Cell<Option<u64>>,

    bookmarks: Bookmarks,
    bookmarks_path: Option<PathBuf>,
    history: History,
    history_path: Option<PathBuf>,
    newtab: Arc<RwLock<NewTabState>>,

    /// The engine's latest answer to "which process draws which page".
    renderers: Vec<RendererInfo>,
    renderer_query_pending: bool,
    /// The tab under the mouse whose hover card is showing, and where.
    hover: Option<(TabId, f64, f64)>,
    /// The window is minimized (pages are told to use less memory).
    minimized: bool,
    /// Last total memory figure shown on the toolbar's memory button.
    shown_total: Cell<u64>,

    update: UpdateStatus,
    /// The newer release found by the last check.
    update_release: Option<Release>,
    /// When the next automatic update check is due.
    next_update_check: Instant,
    /// Installed with the installer, so it can update itself.
    can_update_itself: bool,
    /// Installed memory, and whether this is a low-end PC (see `device.rs`).
    device_memory: Option<u64>,
    low_end_device: bool,
}

impl Browser {
    /// `data_dir` holds cookies, cache, settings and the downloads list.
    /// `restored_after_crash`: reopen the last session no matter what the
    /// startup setting says, because the engine crashed a moment ago.
    pub fn new(
        target: &EventLoopWindowTarget<UserEvent>,
        window: Window,
        proxy: EventLoopProxy<UserEvent>,
        data_dir: Option<PathBuf>,
        restored_after_crash: bool,
    ) -> wry::Result<Self> {
        let settings_path = data_dir.as_ref().map(|dir| dir.join("settings.json"));
        let downloads_path = data_dir.as_ref().map(|dir| dir.join("downloads.json"));
        let session_path = data_dir.as_ref().map(|dir| dir.join("session.json"));
        let bookmarks_path = data_dir.as_ref().map(|dir| dir.join("bookmarks.json"));
        let history_path = data_dir.as_ref().map(|dir| dir.join("history.json"));

        let settings = settings_path.as_deref().map(Settings::load).unwrap_or_default();
        let downloads = downloads_path.as_deref().map(Downloads::load).unwrap_or_default();
        let bookmarks = bookmarks_path.as_deref().map(Bookmarks::load).unwrap_or_default();
        let history = history_path.as_deref().map(History::load).unwrap_or_default();
        let device_memory = device::total_memory();
        let low_end_device = device::is_low_end(device_memory, device::processor_count());
        let lite = match settings.efficiency_mode {
            EfficiencyMode::On => true,
            EfficiencyMode::Off => false,
            EfficiencyMode::Auto => low_end_device,
        };
        let newtab = Arc::new(RwLock::new(NewTabState::from_sources(&settings, &bookmarks, &history, lite)));

        crate::trace("settings, bookmarks and history loaded");
        let mut web_context = WebContext::new(data_dir);
        window.set_theme(window_theme(settings.theme));

        let ui_proxy = proxy.clone();
        let chrome_height = chrome_height_for(&settings);
        let refresh_hz = display::monitor_refresh_hz(window.hwnd());
        let chrome = WebViewBuilder::new_with_web_context(&mut web_context)
            .with_html(UI_HTML)
            .with_bounds(chrome_bounds(&window, false, chrome_height))
            // The theme is shared by every web view of the profile: it sets
            // the UI's colours and what pages see as the preferred scheme.
            .with_theme(webview_theme(settings.theme))
            .with_additional_browser_args(display::BROWSER_ARGS)
            .with_ipc_handler(move |request| {
                if let Ok(msg) = serde_json::from_str::<UiMsg>(request.body()) {
                    let _ = ui_proxy.send_event(UserEvent::Ui(msg));
                }
            })
            // The UI is not a web page: it never opens windows or accepts dropped
            // files, and WebView2's own shortcuts (reload, zoom, print, find,
            // its downloads popup on Ctrl+J) must not act on the toolbar.
            .with_new_window_req_handler(|_, _| NewWindowResponse::Deny)
            .with_drag_drop_handler(|_| true)
            .with_browser_accelerator_keys(false)
            .with_devtools(cfg!(debug_assertions))
            .build_as_child(&window)?;
        log_err(webview_hooks::configure_ui_webview(&chrome), "configure UI webview");
        if let Err(e) = webview_hooks::attach_ui_crash(&chrome, proxy.clone()) {
            eprintln!("watch browser UI for crashes: {e}");
        }

        let menu = PopupMenu::new(
            target,
            &window,
            chrome.environment(),
            window_theme(settings.theme),
            webview_theme(settings.theme),
            proxy.clone(),
        )?;

        Ok(Self {
            download_hook: DownloadHook::new(downloads.next_id(), proxy.clone(), settings.download_dir.clone()),
            tabs: Vec::new(),
            background: Vec::new(),
            chrome,
            menu,
            web_context,
            window,
            active: None,
            split: None,
            next_id: 1,
            proxy,
            maximized: false,
            fullscreen_tab: None,
            window_fullscreen: false,
            exit_requested: false,
            confirm_close: false,
            closed_tabs: Vec::new(),
            block_popups: Rc::new(Cell::new(settings.block_popups)),
            block_ads: Rc::new(Cell::new(settings.block_ads)),
            notice_action: None,
            restarting: false,
            started_at: Instant::now(),
            restored_after_crash,
            status_text: String::new(),
            settings,
            settings_path,
            session_path,
            saved_session: RefCell::new(None),
            notice: None,
            engine_version: wry::webview_version().unwrap_or_default(),
            refresh_hz,
            panel: None,
            downloads,
            downloads_path,
            download_tabs: HashMap::new(),
            cancelled: HashSet::new(),
            downloads_unseen: false,
            downloads_added_seq: 0,
            last_progress_sync: Instant::now(),
            sync_pending: Cell::new(false),
            last_title: RefCell::new(String::new()),
            bookmarks_rev: 1,
            sent_bookmarks_rev: Cell::new(None),
            bookmarks,
            bookmarks_path,
            history,
            history_path,
            newtab,
            renderers: Vec::new(),
            renderer_query_pending: false,
            hover: None,
            minimized: false,
            shown_total: Cell::new(0),
            update: UpdateStatus::Idle,
            update_release: None,
            next_update_check: Instant::now() + FIRST_UPDATE_CHECK,
            can_update_itself: updater::can_update_itself(),
            device_memory,
            low_end_device,
        })
    }

    /// Efficiency mode is on: chosen in Settings, or "Auto" on a low-end PC.
    fn efficiency_active(&self) -> bool {
        match self.settings.efficiency_mode {
            EfficiencyMode::On => true,
            EfficiencyMode::Off => false,
            EfficiencyMode::Auto => self.low_end_device,
        }
    }

    /// Starts the clock for the memory saver: a `Tick` event every few seconds.
    /// The thread ends with the browser (when the event can't be delivered).
    pub fn start_memory_saver(&self) {
        let proxy = self.proxy.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(5));
            if proxy.send_event(UserEvent::Tick).is_err() {
                break;
            }
        });
    }

    /// Opens the first tab(s): `input` (a URL or search from the command
    /// line) if given, otherwise whatever "On startup" in Settings says.
    pub fn open_initial_tabs(&mut self, input: Option<&str>) -> wry::Result<()> {
        let from_command_line =
            input.and_then(|text| url_input::resolve(text, self.settings.search_engine.search_prefix()));
        if let Some(url) = from_command_line {
            return self.open_tab(&url, true, Placement::End, false).map(|_| ());
        }

        if self.settings.startup == Startup::Continue || self.restored_after_crash {
            let session = self.session_path.as_deref().map(Session::load).unwrap_or_default();
            if !session.tabs.is_empty() {
                let mut active = None;
                for (index, url) in session.tabs.iter().enumerate() {
                    let title = session.titles.get(index).cloned().unwrap_or_default();
                    let id = self.push_lazy_tab(url, title, Placement::End, false);
                    if index == session.active {
                        active = Some(id);
                    }
                }
                if let Some(first) = self.tabs.first().map(|t| t.id) {
                    self.select_tab(active.unwrap_or(first));
                    return Ok(());
                }
            }
        }

        let url = match self.settings.startup {
            Startup::NewTab => self.new_tab_url(),
            Startup::HomePage | Startup::Continue => self.settings.home_url(),
        };
        self.open_tab(&url, true, Placement::End, false).map(|_| ())
    }

    /// Re-fits every web view to the window. Call on resize and DPI change.
    pub fn layout(&mut self) {
        let size = self.window.inner_size();
        if size.width == 0 || size.height == 0 {
            // Minimized: nothing is on screen, so even the visible pages can
            // use less memory (Microsoft's advice for WebView2 apps).
            if !self.minimized {
                self.minimized = true;
                for tab in self.tabs.iter().filter(|t| t.wv_visible.get()) {
                    tab.set_webview_low_memory(true);
                }
            }
            return;
        }
        if self.minimized {
            self.minimized = false;
            for tab in self.tabs.iter().filter(|t| t.wv_visible.get()) {
                tab.set_webview_low_memory(false);
            }
        }
        if !self.is_fullscreen() {
            let full_chrome = self.chrome_needs_full();
            log_err(
                self.chrome.set_bounds(chrome_bounds(&self.window, full_chrome, self.chrome_height())),
                "resize UI",
            );
        }
        let (split_bounds, full_bounds) = self.visible_pane_layout();
        for tab in &self.tabs {
            if tab.webview.is_none() {
                tab.applied_bounds.set(None);
                continue;
            }
            if let Some(b) = pane_bounds_for_tab(split_bounds, full_bounds, self.active, tab.id) {
                tab.apply_webview_bounds(b);
            } else {
                // Defer resizing hidden background tabs until they become active.
                tab.applied_bounds.set(None);
            }
        }

        // Maximizing or restoring also arrives here as a resize.
        let maximized = self.window.is_maximized();
        if maximized != self.maximized {
            self.maximized = maximized;
            self.sync();
        }
    }

    /// Re-reads the active monitor's refresh rate after moving/resizing across
    /// displays and syncs to the UI only if the frequency changed.
    pub fn update_display_rate(&mut self) {
        let hz = display::monitor_refresh_hz(self.window.hwnd());
        if hz != self.refresh_hz {
            self.refresh_hz = hz;
            self.sync();
        }
    }

    /// Attempts to close the browser; asks for confirmation if downloads are running.
    pub fn request_close(&mut self) {
        if self.downloads.active_count() > 0 && !self.confirm_close {
            if self.fullscreen_tab.is_some() {
                self.exit_fullscreen();
            }
            self.confirm_close = true;
            self.stretch_chrome(true);
            self.sync();
        } else {
            self.exit_requested = true;
        }
    }

    /// True once the user confirmed closing (or closed with no active downloads).
    pub fn exit_requested(&self) -> bool {
        self.exit_requested
    }

    /// Whether `id` belongs to the floating popup menu window.
    pub fn is_menu_window(&self, id: WindowId) -> bool {
        self.menu.is_window(id)
    }

    /// True when the popup menu window or its WebView2 child owns focus.
    pub fn menu_owns_focus(&self) -> bool {
        self.menu.owns_focus()
    }

    /// Closes any open popup menu.
    pub fn hide_menu(&mut self) {
        self.menu.hide();
    }

    fn window_action(&mut self, action: WindowAction) {
        let resize = |direction| {
            // The top edge is only a resize handle while the window isn't maximized.
            if !self.window.is_maximized() {
                if let Err(e) = self.window.drag_resize_window(direction) {
                    eprintln!("resize window: {e}");
                }
            }
        };
        match action {
            WindowAction::Minimize => self.window.set_minimized(true),
            WindowAction::ToggleMaximize => self.window.set_maximized(!self.window.is_maximized()),
            WindowAction::Close => self.request_close(),
            // Hands the drag to Windows, so snapping to screen edges, dragging
            // a maximized window down to restore it, and so on all just work.
            WindowAction::Drag => {
                if let Err(e) = self.window.drag_window() {
                    eprintln!("drag window: {e}");
                }
            }
            WindowAction::ResizeTop => resize(ResizeDirection::North),
            WindowAction::ResizeTopLeft => resize(ResizeDirection::NorthWest),
            WindowAction::ResizeTopRight => resize(ResizeDirection::NorthEast),
        }
    }

    pub fn handle(&mut self, event: UserEvent) {
        match event {
            UserEvent::Ui(msg) => self.handle_ui(msg),
            UserEvent::Shortcut(id, action) => self.handle_shortcut(id, action),
            UserEvent::TitleChanged(id, title) => {
                let mut changed = false;
                let mut url_for_history = None;
                if let Some(tab) = self.tab_mut(id) {
                    let next_title = if newtab::is_internal_url(&tab.url) { String::new() } else { title.clone() };
                    if tab.title != next_title {
                        changed = true;
                        tab.title = next_title;
                        // Private tabs leave no trace in history.
                        if !tab.private && !tab.title.is_empty() {
                            url_for_history = Some(tab.url.clone());
                        }
                    }
                }
                if let Some(url) = url_for_history {
                    if self.history.update_title(&url, &title) {
                        self.save_history();
                    }
                }
                if changed {
                    self.sync();
                }
            }
            UserEvent::PageLoad { id, finished, url } => {
                crate::trace(&format!("tab {id}: page {} {url}", if finished { "loaded" } else { "started" }));
                let mut changed = false;
                let mut record_visit = None;
                let norm_url = newtab::normalize_url(&url);
                if let Some(tab) = self.tab_mut(id) {
                    let is_ntp = norm_url == NEW_TAB_URL || (norm_url.is_empty() && tab.url == NEW_TAB_URL);
                    let loading = !finished && !is_ntp;
                    // (The ad count is reset by the blocker itself when a new page starts.)
                    if finished && tab.ads_blocked.get() > 0 {
                        changed = true; // show the page's count
                    }
                    if tab.loading != loading {
                        tab.loading = loading;
                        changed = true;
                    }
                    if !norm_url.is_empty() && tab.url != norm_url {
                        tab.url = norm_url;
                        if tab.url == NEW_TAB_URL && !tab.title.is_empty() {
                            tab.title.clear();
                        }
                        changed = true;
                    }
                    if finished && !tab.private && tab.url != NEW_TAB_URL {
                        record_visit = Some((tab.url.clone(), tab.title.clone()));
                    }
                }
                self.apply_site_zoom(id);
                if let Some((u, t)) = record_visit {
                    if self.history.record(&u, &t) {
                        self.save_history();
                    }
                }
                if changed {
                    self.sync();
                }
            }
            UserEvent::HistoryChanged { id, can_go_back, can_go_forward } => {
                if let Some(tab) = self.tab_mut(id) {
                    if tab.can_go_back != can_go_back || tab.can_go_forward != can_go_forward {
                        tab.can_go_back = can_go_back;
                        tab.can_go_forward = can_go_forward;
                        if self.active == Some(id) {
                            self.sync();
                        }
                    }
                }
            }
            UserEvent::FaviconChanged { id, url } => {
                if let Some(tab) = self.tab_mut(id) {
                    if tab.favicon != url {
                        tab.favicon = url;
                        self.sync();
                    }
                }
            }
            UserEvent::StatusTextChanged { id, text } => {
                if self.active == Some(id) && self.status_text != text {
                    self.status_text = text;
                    if !self.sync_pending.get() {
                        if let Ok(json) = serde_json::to_string(&self.status_text) {
                            log_err(
                                self.chrome
                                    .evaluate_script(&format!("window.browser && window.browser.setStatus({json})")),
                                "update status",
                            );
                        }
                    }
                }
            }
            UserEvent::FullScreenChanged { id, fullscreen } => {
                if self.active == Some(id) {
                    if fullscreen {
                        self.enter_fullscreen(id);
                    } else if self.fullscreen_tab == Some(id) {
                        // The page left full screen by itself (e.g. Esc).
                        // Stay full screen if the user had chosen F11.
                        self.fullscreen_tab = None;
                        self.apply_fullscreen();
                    }
                }
            }
            UserEvent::ZoomChanged { id, factor } => {
                let pct = (factor * 100.0).round().clamp(25.0, 500.0) as u16;
                let mut site = None;
                if let Some(tab) = self.tab_mut(id) {
                    if !tab.private {
                        site = settings::zoom_site(&tab.url);
                    }
                    if tab.zoom_percent != pct {
                        tab.zoom_percent = pct;
                        if self.active == Some(id) {
                            self.sync();
                            if self.menu.is_visible() && self.menu.kind() == Some(MenuKind::Main) {
                                self.refresh_main_menu();
                            }
                        }
                    }
                }
                // Remember it for the site (private tabs leave no trace).
                if let Some(site) = site {
                    if self.settings.remember_site_zoom(&site, pct) {
                        self.save_settings_quietly();
                    }
                }
            }
            UserEvent::OpenInNewTab { from, url } => {
                // Only real web links may open tabs; never `javascript:` and friends.
                // A link from a private tab opens in another private tab.
                if is_web_url(&url) {
                    let private = self.tab(from).is_some_and(|t| t.private);
                    self.open_tab_logged(&url, Placement::AfterTab(from), private);
                }
            }
            UserEvent::PopupBlocked { from, url } => {
                if self.active == Some(from) && is_web_url(&url) {
                    let private = self.tab(from).is_some_and(|t| t.private);
                    self.show_notice_with_action("Pop-up blocked", "Open", NoticeAction::OpenUrl { url, private });
                }
            }
            UserEvent::AudioChanged { id, playing, muted } => {
                if let Some(tab) = self.tab_mut(id) {
                    if (tab.audible, tab.muted) != (playing, muted) {
                        tab.audible = playing;
                        tab.muted = muted;
                        self.sync();
                    }
                }
            }
            UserEvent::ProcessFailed { id, failure } => self.process_failed(id, failure),
            UserEvent::UiProcessFailed(failure) => match failure {
                // Reload the UI; when it's back it asks for the state again.
                ProcessFailure::PageCrashed | ProcessFailure::PageUnresponsive => {
                    log_err(self.chrome.load_html(UI_HTML), "reload browser UI");
                }
                ProcessFailure::EngineExited => self.restart_after_engine_crash(),
                ProcessFailure::Other => {}
            },
            UserEvent::TabFocused(id) => {
                if !self.menu.is_visible()
                    && self.split_active_pair().is_some_and(|(l, r)| l == id || r == id)
                    && self.active != Some(id)
                    && !self.chrome_needs_full()
                    && self.tab(id).is_some()
                {
                    self.active = Some(id);
                    self.status_text.clear();
                    self.sync();
                }
            }
            UserEvent::MenuReady => self.menu.on_ready(),
            UserEvent::Screenshot { id, result } => match result {
                Ok(image) => self.save_screenshot(id, image),
                Err(e) => self.show_notice(&format!("Couldn't take a screenshot: {e}"), true),
            },
            UserEvent::AdsBlocked(id) => {
                if let Some(tab) = self.tab(id) {
                    tab.ads_notified.set(false);
                    if self.on_screen_tabs().contains(&id) {
                        self.sync();
                    }
                }
            }
            UserEvent::ReaderResult { id, result } => {
                if result == "none" && self.active == Some(id) {
                    self.show_notice("Reader mode isn't available: this page doesn't look like an article", false);
                }
            }
            UserEvent::MenuLostFocus => {
                if self.menu.is_visible() && !self.menu.owns_focus() {
                    self.hide_menu();
                }
            }
            UserEvent::Tick => {
                self.manage_memory();
                self.update_memory_button(false);
                self.check_for_updates_if_due();
                self.menu.release_if_idle();
                #[cfg(debug_assertions)]
                self.debug_memory_probe();
            }
            UserEvent::RendererInfos(renderers) => {
                self.renderers = renderers;
                self.renderer_query_pending = false;
                self.refresh_card();
                self.refresh_memory_panel();
                #[cfg(debug_assertions)]
                self.debug_memory_report();
            }
            UserEvent::FindFailed(id) => {
                // The find API isn't available: press Ctrl+F in the page instead.
                if self.active == Some(id) {
                    if let Some(wv) = self.active_webview() {
                        log_err(wv.focus(), "focus page for find");
                        webview_hooks::press_ctrl_f();
                    }
                }
            }
            UserEvent::Update(event) => self.on_update_event(event),
            UserEvent::Imported { source, result } => self.finish_import(source, result),
            UserEvent::ExternalOpen(arg) => {
                self.window.set_minimized(false);
                self.window.set_focus();
                if let Some(input) = arg {
                    if let Some(url) = url_input::resolve(&input, self.settings.search_engine.search_prefix()) {
                        self.open_tab_logged(&url, Placement::End, false);
                    }
                }
            }
            UserEvent::DownloadStarted { id, tab, url, path, total } => {
                let private = self.tabs.iter().chain(&self.background).find(|t| t.id == tab).is_some_and(|t| t.private);
                self.download_tabs.insert(id, tab);
                let download =
                    Download { id, url, path, received: 0, total, state: DownloadState::InProgress, private };
                self.downloads.start(download);
                self.downloads_added_seq = self.downloads_added_seq.wrapping_add(1);
                self.downloads_changed();
            }
            UserEvent::DownloadProgress { id, received, total } => {
                // Progress arrives very often; throttle UI updates cleanly.
                let running = self.downloads.progress(id, received, total);
                if running && self.last_progress_sync.elapsed() >= PROGRESS_INTERVAL {
                    self.last_progress_sync = Instant::now();
                    self.sync();
                }
            }
            UserEvent::DownloadFinished { id, completed, path, received } => {
                let state = if completed {
                    DownloadState::Completed
                } else if self.cancelled.contains(&id) {
                    DownloadState::Cancelled
                } else {
                    DownloadState::Failed
                };
                self.cancelled.remove(&id);
                self.downloads.finish(id, state, path, received);
                self.download_tabs.remove(&id);
                self.release_background_tabs();
                self.downloads_changed();
            }
        }
    }

    fn handle_ui(&mut self, msg: UiMsg) {
        match msg {
            UiMsg::Ready => {
                crate::trace("browser UI loaded");
                self.send_init();
                self.sync();
                self.update_memory_button(true);
                if self.active_tab().is_some_and(|t| t.url == NEW_TAB_URL) {
                    self.focus_address();
                }
            }
            UiMsg::Navigate { input } => self.navigate(&input),
            UiMsg::NewTab => self.new_tab(false),
            UiMsg::Command { command } => self.run_command(command),
            UiMsg::MenuCommand { command, close } => {
                if close {
                    self.menu.hide();
                }
                self.run_command(command);
                if !close {
                    self.refresh_main_menu();
                }
            }
            UiMsg::TabMenuCommand { id, command } => {
                self.menu.hide();
                self.run_tab_command(id, command);
            }
            UiMsg::CloseMenu => {
                self.menu.hide();
                self.window.set_focus();
                self.focus_content();
            }
            UiMsg::ShowMenu { x, y } => self.show_main_menu(x, y),
            UiMsg::TabMenu { id, x, y } => self.show_tab_menu(id, x, y),
            UiMsg::ToggleMute { id } => self.toggle_mute(id),
            UiMsg::TabHover { id, x, y } => {
                self.hover = Some((id, x, y));
                self.refresh_card();
                // Ask which processes draw which page; the card updates when
                // the answer arrives. Repeats each second while hovering.
                if !self.renderer_query_pending && self.tab(id).is_some_and(|t| t.webview.is_some()) {
                    match webview_hooks::query_renderers(&self.chrome, self.proxy.clone()) {
                        Ok(()) => self.renderer_query_pending = true,
                        Err(e) => eprintln!("ask engine for processes: {e}"),
                    }
                }
            }
            UiMsg::TabHoverEnd => {
                self.hover = None;
                self.menu.hide_card();
            }
            UiMsg::ShowMemoryPanel { x, y } => self.show_memory_panel(x, y),
            UiMsg::MemoryPanelRefresh => {
                if self.menu.is_visible() && self.menu.kind() == Some(MenuKind::Memory) {
                    self.request_renderers();
                    self.refresh_memory_panel();
                }
            }
            UiMsg::SleepTab { id } => {
                if self.tab(id).is_some_and(|t| self.can_sleep(t)) {
                    self.sleep_tabs(&[id]);
                }
            }
            UiMsg::SleepBackgroundTabs => {
                let ids: Vec<TabId> = self.tabs.iter().filter(|t| self.can_bulk_sleep(t)).map(|t| t.id).collect();
                if ids.is_empty() {
                    self.show_notice("No background tabs to put to sleep", false);
                } else {
                    self.sleep_tabs(&ids);
                }
            }
            UiMsg::NoticeAction { id } => {
                if self.notice.as_ref().is_some_and(|n| n.id == id) {
                    if let Some(action) = self.notice_action.take() {
                        self.run_notice_action(action);
                    }
                }
            }
            UiMsg::CheckForUpdates => self.check_for_updates(true),
            UiMsg::ResetSiteZoom => self.reset_site_zoom(),
            UiMsg::ImportFrom { source } => self.import_from(source),
            UiMsg::InstallUpdate => self.install_update(),
            UiMsg::CloseTab { id } => self.close_tab(id),
            UiMsg::SelectTab { id, focus } => self.select_tab_with_focus(id, focus),
            UiMsg::ReorderTab { id, to_index } => self.reorder_tab(id, to_index),
            UiMsg::ReopenClosedTab => self.reopen_closed_tab(),
            UiMsg::Back => {
                if let Some(wv) = self.active_webview() {
                    if let Err(e) = webview_hooks::go_back(wv) {
                        self.show_notice(&format!("Couldn't go back: {e}"), true);
                    }
                }
            }
            UiMsg::Forward => {
                if let Some(wv) = self.active_webview() {
                    if let Err(e) = webview_hooks::go_forward(wv) {
                        self.show_notice(&format!("Couldn't go forward: {e}"), true);
                    }
                }
            }
            UiMsg::Reload => {
                if let Some(wv) = self.active_webview() {
                    log_err(wv.reload(), "reload");
                }
            }
            UiMsg::Stop => {
                if let Some(wv) = self.active_webview() {
                    log_err(webview_hooks::stop(wv), "stop");
                }
            }
            UiMsg::ResetZoom => self.reset_active_zoom(),
            UiMsg::ToggleBookmark => self.toggle_active_bookmark(),
            UiMsg::RemoveBookmark { url } => {
                if self.bookmarks.remove(&url) {
                    self.bookmarks_rev += 1;
                    self.save_bookmarks();
                    self.sync();
                }
            }
            UiMsg::OpenUrl { url, new_tab } => {
                // These come from the bookmarks and history lists, which are
                // loaded from files on disk. Only plain web and file addresses
                // are opened; a `javascript:` entry in an edited file is not.
                if !crate::session::is_restorable(&url) {
                    self.show_notice("That address can't be opened", true);
                } else if new_tab {
                    self.open_tab_logged(&url, Placement::End, false);
                } else {
                    self.load_in_active(url);
                }
            }
            UiMsg::RemoveHistory { url } => {
                if self.history.remove(&url) {
                    self.save_history();
                    self.sync();
                }
            }
            UiMsg::ClearHistory => {
                if self.history.clear() {
                    self.save_history();
                }
                self.show_notice("Browsing history cleared", false);
            }
            UiMsg::QuerySuggestions { query } => self.send_suggestions(&query),
            UiMsg::ConfirmClose => self.exit_requested = true,
            UiMsg::CancelClose => {
                self.confirm_close = false;
                self.stretch_chrome(self.chrome_needs_full());
                self.sync();
                self.focus_content();
            }
            UiMsg::FocusContent => self.focus_active_webview(),
            UiMsg::TogglePanel { panel } => {
                if panel == Panel::Downloads {
                    self.open_downloads_tab();
                } else {
                    let next = if self.panel == Some(panel) { None } else { Some(panel) };
                    self.set_panel(next);
                }
            }
            UiMsg::ClosePanel => self.set_panel(None),
            UiMsg::GoHome => {
                let url = self.settings.home_url();
                self.load_in_active(url);
            }
            UiMsg::UpdateSettings { patch } => self.update_settings(patch),
            UiMsg::ChooseDownloadFolder => self.choose_download_folder(),
            UiMsg::ResetDownloadFolder => self.set_download_folder(None),
            UiMsg::ClearBrowsingData => self.clear_browsing_data(),
            UiMsg::ResetSettings => {
                let before = std::mem::take(&mut self.settings);
                self.settings_changed(&before);
                self.show_notice("Settings reset to their defaults", false);
            }
            UiMsg::DownloadAction { id, action } => self.download_action(id, action),
            UiMsg::ClearDownloads => {
                if self.downloads.clear_finished() {
                    self.save_downloads();
                }
                self.sync();
            }
            UiMsg::OpenDownloadsFolder => self.open_downloads_folder(),
            UiMsg::Window { action } => self.window_action(action),
        }
    }

    fn handle_shortcut(&mut self, sender: TabId, action: Shortcut) {
        // Only visible tabs (the active tab or its split partner) can send
        // shortcuts. Promote the split partner immediately so GotFocus ordering
        // can never drop a shortcut pressed in either pane.
        let in_visible_split = self.split_active_pair().is_some_and(|(l, r)| l == sender || r == sender);
        if self.active != Some(sender) {
            if in_visible_split {
                self.active = Some(sender);
            } else {
                return;
            }
        }
        match action {
            Shortcut::NewTab => self.new_tab(false),
            Shortcut::NewPrivateTab => self.new_tab(true),
            Shortcut::CloseTab => self.close_tab(sender),
            Shortcut::ReopenTab => self.reopen_closed_tab(),
            Shortcut::FocusAddress => {
                if self.is_fullscreen() {
                    self.exit_fullscreen();
                }
                self.focus_address();
            }
            Shortcut::Bookmarks => self.toggle_panel_from_shortcut(Panel::Bookmarks),
            Shortcut::ToggleBookmarksBar => self.toggle_bookmarks_bar(),
            Shortcut::Fullscreen => self.run_command(BrowserCommand::Fullscreen),
            Shortcut::Reader => self.run_command(BrowserCommand::ReaderMode),
            Shortcut::SearchTabs => self.run_command(BrowserCommand::SearchTabs),
            Shortcut::Screenshot => self.run_command(BrowserCommand::Screenshot),
            Shortcut::SavePage => self.run_command(BrowserCommand::SavePage),
            Shortcut::Menu => {
                if self.is_fullscreen() {
                    self.exit_fullscreen();
                }
                // The UI knows where its menu button is; it asks for the menu.
                log_err(self.chrome.focus(), "focus UI");
                self.flush_sync();
                log_err(self.chrome.evaluate_script("window.browser && window.browser.openMenu()"), "open menu");
            }
            Shortcut::NextTab => self.cycle_tab(1),
            Shortcut::PrevTab => self.cycle_tab(-1),
            Shortcut::SelectTab(index) => {
                if let Some(tab) = self.tabs.get(index) {
                    let id = tab.id;
                    self.select_tab(id);
                }
            }
            Shortcut::LastTab => {
                if let Some(tab) = self.tabs.last() {
                    let id = tab.id;
                    self.select_tab(id);
                }
            }
            Shortcut::Downloads => self.open_downloads_tab(),
            Shortcut::Settings => self.toggle_panel_from_shortcut(Panel::Settings),
            Shortcut::History => self.toggle_panel_from_shortcut(Panel::History),
            Shortcut::Bookmark => self.toggle_active_bookmark(),
            Shortcut::Home => {
                let url = self.settings.home_url();
                self.load_in_active(url);
            }
            Shortcut::ResetZoom => self.reset_active_zoom(),
        }
    }

    fn toggle_panel_from_shortcut(&mut self, panel: Panel) {
        if self.is_fullscreen() {
            self.exit_fullscreen();
        }
        let next = if self.panel == Some(panel) { None } else { Some(panel) };
        self.set_panel(next);
    }

    // ---- commands and menus ---------------------------------------------

    /// Runs a command from the main menu, a keyboard shortcut or the UI.
    fn run_command(&mut self, command: BrowserCommand) {
        use BrowserCommand as C;
        match command {
            C::NewTab => self.new_tab(false),
            C::NewPrivateTab => self.new_tab(true),
            C::ReopenClosedTab => self.reopen_closed_tab(),
            C::ToggleSplitView => self.toggle_split_view(),
            C::SwapSplitView => self.swap_split_view(),
            C::ToggleBookmark => self.toggle_active_bookmark(),
            C::ToggleBookmarksBar => self.toggle_bookmarks_bar(),
            C::Bookmarks => self.toggle_panel_from_shortcut(Panel::Bookmarks),
            C::History => self.toggle_panel_from_shortcut(Panel::History),
            C::Downloads => self.open_downloads_tab(),
            C::Settings => self.toggle_panel_from_shortcut(Panel::Settings),
            C::ZoomIn => self.zoom_active(true),
            C::ZoomOut => self.zoom_active(false),
            C::ResetZoom => self.reset_active_zoom(),
            C::Fullscreen => self.toggle_window_fullscreen(),
            C::ReaderMode => self.toggle_reader(),
            C::SearchTabs => self.show_tab_search(),
            C::Screenshot => self.take_screenshot(),
            C::ImportData => self.import_from_other_browsers(),
            C::Exit => self.request_close(),
            // These act on the page, or filter when viewing Downloads / Bookmarks / History.
            C::Find if matches!(self.panel, Some(Panel::Bookmarks | Panel::History)) => {
                log_err(self.chrome.focus(), "focus UI");
                self.flush_sync();
                log_err(self.chrome.evaluate_script("window.browser && window.browser.focusFilter()"), "focus filter");
            }
            C::Find if self.panel.is_none() && self.active_is_downloads() => {
                log_err(self.chrome.focus(), "focus UI");
                self.flush_sync();
                log_err(self.chrome.evaluate_script("window.browser && window.browser.focusFilter()"), "focus filter");
            }
            C::Find | C::Print | C::SavePage | C::DevTools => {
                if self.active_webview().is_none() {
                    return;
                }
                self.set_panel(None);
                let Some(id) = self.active else { return };
                let Some(wv) = self.active_webview() else { return };
                let result = match command {
                    C::Find => {
                        log_err(wv.focus(), "focus page for find");
                        webview_hooks::find(wv, id, self.proxy.clone())
                    }
                    C::Print => webview_hooks::print(wv),
                    C::SavePage => webview_hooks::save_page(wv),
                    _ => webview_hooks::open_devtools(wv),
                };
                if let Err(e) = result {
                    if command == C::Find {
                        // Fall back to pressing Ctrl+F for the user.
                        webview_hooks::press_ctrl_f();
                    } else {
                        self.show_notice(&format!("That isn't available here: {e}"), true);
                    }
                }
            }
        }
    }

    /// Converts a point in the UI web view (CSS pixels) to screen pixels.
    fn to_screen(&self, x: f64, y: f64) -> (i32, i32) {
        let origin = self.window.inner_position().unwrap_or_default();
        let scale = self.window.scale_factor();
        (origin.x + (x * scale).round() as i32, origin.y + (y * scale).round() as i32)
    }

    fn main_menu_entries(&self) -> Vec<MenuEntry> {
        use BrowserCommand as C;
        let tab = self.active_tab();
        let has_page = tab.is_some_and(|t| t.url != NEW_TAB_URL && !is_downloads_url(&t.url) && !t.url.is_empty());
        let has_view = tab.is_some_and(|t| t.webview.is_some());
        let bookmarked = tab.is_some_and(|t| self.bookmarks.contains(&t.url));
        let zoom = tab.map_or(100, |t| t.zoom_percent);
        let fullscreen = self.is_fullscreen();
        let in_split = self.split_active_pair().is_some();
        let can_split = in_split || tab.is_some_and(|t| !is_downloads_url(&t.url));

        let mut entries = vec![
            MenuEntry::item(C::NewTab, "New tab", "Ctrl+T", "newTab", true),
            MenuEntry::item(C::NewPrivateTab, "New private tab", "Ctrl+Shift+N", "privateTab", true),
            MenuEntry::item(
                C::ReopenClosedTab,
                "Reopen closed tab",
                "Ctrl+Shift+T",
                "reopen",
                !self.closed_tabs.is_empty(),
            ),
            MenuEntry::item(C::SearchTabs, "Search tabs…", "Ctrl+Shift+A", "searchTabs", true),
            MenuEntry::item(
                C::ToggleSplitView,
                if in_split { "Close split view" } else { "Split view" },
                "",
                "split",
                can_split,
            ),
        ];
        if in_split {
            entries.push(MenuEntry::item(C::SwapSplitView, "Swap split panes", "", "swap", true));
        }
        entries.extend([
            MenuEntry::Sep,
            MenuEntry::item(
                C::ToggleBookmark,
                if bookmarked { "Remove bookmark" } else { "Bookmark this page" },
                "Ctrl+D",
                "bookmark",
                has_page,
            ),
            MenuEntry::item(C::Bookmarks, "Bookmarks", "Ctrl+Shift+O", "bookmarks", true),
            MenuEntry::toggle_item(
                C::ToggleBookmarksBar,
                "Show bookmarks bar",
                "Ctrl+Shift+B",
                self.settings.show_bookmarks_bar,
                true,
            ),
            MenuEntry::item(C::History, "History", "Ctrl+H", "history", true),
            MenuEntry::item(C::Downloads, "Downloads", "Ctrl+J", "downloads", true),
            MenuEntry::Sep,
            MenuEntry::Zoom {
                percent: zoom,
                can_in: has_view && settings::zoom_step(zoom, true) != zoom,
                can_out: has_view && settings::zoom_step(zoom, false) != zoom,
                has_view,
                fullscreen,
            },
            MenuEntry::Sep,
            MenuEntry::item(
                C::Find,
                "Find on page…",
                "Ctrl+F",
                "find",
                has_view
                    || matches!(self.panel, Some(Panel::Bookmarks | Panel::History))
                    || (self.panel.is_none() && self.active_is_downloads()),
            ),
            MenuEntry::item(
                C::ReaderMode,
                "Reader mode",
                "F9",
                "reader",
                has_view && self.active_tab().is_some_and(|t| is_web_page(&t.url)),
            ),
            MenuEntry::item(
                C::Screenshot,
                "Screenshot page",
                "Ctrl+Shift+S",
                "screenshot",
                has_view && self.active_tab().is_some_and(|t| is_web_page(&t.url)),
            ),
            MenuEntry::item(C::Print, "Print…", "Ctrl+P", "print", has_page && has_view),
            MenuEntry::item(C::SavePage, "Save page as…", "Ctrl+S", "save", has_page && has_view),
            MenuEntry::item(C::DevTools, "Developer tools", "F12", "devtools", has_view),
            MenuEntry::Sep,
            MenuEntry::item(C::ImportData, "Import from Chrome or Edge…", "", "import", true),
            MenuEntry::item(C::Settings, "Settings", "Ctrl+,", "settings", true),
            MenuEntry::danger(C::Exit, "Exit", "", "exit", true),
        ]);
        entries
    }

    fn refresh_main_menu(&self) {
        let entries = self.main_menu_entries();
        self.menu.update_entries(&entries);
    }

    /// The ⋮ menu. `x`, `y`: bottom-right corner of its button (CSS pixels).
    fn show_main_menu(&mut self, x: f64, y: f64) {
        if (self.menu.is_visible() && self.menu.kind() == Some(MenuKind::Main)) || self.menu.recently_closed() {
            self.menu.hide();
            return;
        }
        let (sx, sy) = self.to_screen(x, y);
        let entries = self.main_menu_entries();
        self.menu.show(&self.window, MenuKind::Main, sx, sy, true, &entries);
    }

    /// Right-click on tab `id`, at `x`, `y` (CSS pixels).
    fn show_tab_menu(&mut self, id: TabId, x: f64, y: f64) {
        use TabCommand as T;
        let Some(index) = self.tabs.iter().position(|t| t.id == id) else { return };
        let tab = &self.tabs[index];
        let (sx, sy) = self.to_screen(x, y);
        let muted = tab.muted;
        let is_dl = is_downloads_url(&tab.url);
        let has_view = tab.webview.is_some();
        let count = self.tabs.len();
        let in_split = self.split_active_pair().is_some_and(|(l, r)| l == id || r == id);
        let can_pair = self.active.is_some_and(|a| {
            a != id
                && !is_downloads_url(&self.tabs[index].url)
                && self.tab(a).is_some_and(|t| !is_downloads_url(&t.url))
        });

        let mut entries = vec![MenuEntry::tab_item(T::NewTabToTheRight, "New tab to the right", "", "newTab", true)];
        if in_split {
            entries.push(MenuEntry::tab_item(T::SwapSplitView, "Swap split panes", "", "swap", true));
            entries.push(MenuEntry::tab_item(T::CloseSplitView, "Close split view", "", "split", true));
        } else {
            entries.push(MenuEntry::tab_item(T::SplitWithActive, "Split with current tab", "", "split", can_pair));
        }
        entries.extend([
            MenuEntry::Sep,
            MenuEntry::tab_item(T::Reload, "Reload", "Ctrl+R", "reload", !is_dl),
            MenuEntry::tab_item(T::Duplicate, "Duplicate", "", "duplicate", !is_dl),
            MenuEntry::tab_item(
                T::ToggleMute,
                if muted { "Unmute tab" } else { "Mute tab" },
                "",
                if muted { "unmute" } else { "mute" },
                has_view,
            ),
            MenuEntry::Sep,
            MenuEntry::tab_item(T::Close, "Close", "Ctrl+W", "close", true),
            MenuEntry::tab_item(T::CloseOthers, "Close other tabs", "", "", count > 1),
            MenuEntry::tab_item(T::CloseToTheRight, "Close tabs to the right", "", "", index + 1 < count),
            MenuEntry::Sep,
            MenuEntry::tab_item(
                T::ReopenClosed,
                "Reopen closed tab",
                "Ctrl+Shift+T",
                "reopen",
                !self.closed_tabs.is_empty(),
            ),
        ]);
        self.menu.show(&self.window, MenuKind::Tab(id), sx, sy, false, &entries);
    }

    fn run_tab_command(&mut self, id: TabId, command: TabCommand) {
        use TabCommand as T;
        let Some(index) = self.tabs.iter().position(|t| t.id == id) else { return };
        match command {
            T::NewTabToTheRight => {
                let url = self.new_tab_url();
                self.open_tab_logged(&url, Placement::AfterTab(id), false);
                self.focus_address();
            }
            T::SplitWithActive => self.split_tab_with_active(id),
            T::SwapSplitView => self.swap_split_view(),
            T::CloseSplitView => self.close_split_view(),
            T::Reload => {
                if self.tab(id).is_some_and(|t| t.webview.is_none()) {
                    // A restored tab that was never opened: opening it loads it.
                    self.select_tab(id);
                } else if let Some(wv) = self.tab(id).and_then(|t| t.webview.as_ref()) {
                    log_err(wv.reload(), "reload");
                }
            }
            T::Duplicate => {
                if let Some((url, private)) = self.tab(id).map(|t| (t.url.clone(), t.private)) {
                    if is_downloads_url(&url) {
                        self.open_downloads_tab();
                    } else {
                        self.open_tab_logged(&url, Placement::AfterTab(id), private);
                    }
                }
            }
            T::ToggleMute => self.toggle_mute(id),
            T::Close => self.close_tab(id),
            T::CloseOthers => {
                let others: Vec<TabId> = self.tabs.iter().map(|t| t.id).filter(|&t| t != id).collect();
                self.select_tab(id);
                for other in others {
                    self.close_tab(other);
                }
            }
            T::CloseToTheRight => {
                let right: Vec<TabId> = self.tabs.iter().skip(index + 1).map(|t| t.id).collect();
                if self.active.is_some_and(|a| right.contains(&a)) {
                    self.select_tab(id);
                }
                for other in right {
                    self.close_tab(other);
                }
            }
            T::ReopenClosed => self.reopen_closed_tab(),
        }
    }

    // ---- memory saver ---------------------------------------------------

    /// Runs every few seconds. Tabs out of sight are first frozen (their
    /// scripts pause, nothing is lost) and, after longer, put to sleep (the
    /// page is unloaded and all its memory freed; it reloads when opened).
    fn manage_memory(&mut self) {
        let saver = match self.settings.memory_saver {
            // Efficiency mode uses the strongest timings (unless the saver is off).
            settings::MemorySaver::Balanced if self.efficiency_active() => settings::MemorySaver::Maximum,
            saver => saver,
        };
        let Some((freeze_after, sleep_after)) = saver.timings() else { return };
        let now = Instant::now();
        // Never touched: what's on screen, what's playing sound, what's downloading.
        let mut keep_awake: HashSet<TabId> = self.download_tabs.values().copied().collect();
        keep_awake.extend(self.active);
        keep_awake.extend(self.fullscreen_tab);
        if let Some((l, r)) = self.split_active_pair() {
            keep_awake.extend([l, r]);
        }

        let mut to_sleep = Vec::new();
        for tab in &self.tabs {
            let Some(wv) = &tab.webview else { continue };
            if tab.wv_visible.get() || tab.audible {
                continue;
            }
            let Some(since) = tab.hidden_since.get() else {
                // Opened in the background and never shown: start counting now.
                tab.hidden_since.set(Some(now));
                continue;
            };
            let hidden_for = now.duration_since(since);
            // Private tabs aren't put to sleep: unloading the last one would
            // throw away its private sign-ins.
            if hidden_for >= sleep_after && !tab.private && !keep_awake.contains(&tab.id) {
                to_sleep.push(tab.id);
            } else if hidden_for >= freeze_after
                && !tab.frozen.get()
                && !self.download_tabs.values().any(|&t| t == tab.id)
                && webview_hooks::try_suspend(wv).is_ok()
            {
                tab.frozen.set(true);
            }
        }
        if !to_sleep.is_empty() {
            for id in to_sleep {
                self.put_to_sleep(id);
            }
            self.sync();
        }
    }

    // ---- memory manager -------------------------------------------------

    /// Everything the browser uses right now: this program plus every
    /// process of its WebView2 engine. (bytes, number of processes)
    fn total_memory(&self) -> (u64, usize) {
        let mut pids: Vec<u32> =
            webview_hooks::engine_processes(&self.chrome).into_iter().map(|(pid, _)| pid).collect();
        pids.push(std::process::id());
        let total = pids.iter().filter_map(|&pid| memory::process_memory(pid)).sum();
        (total, pids.len())
    }

    /// Updates the figure on the toolbar's memory button (if it moved by at
    /// least 1 MB, or always with `force`). Skipped while minimized.
    fn update_memory_button(&self, force: bool) {
        if self.minimized {
            return;
        }
        let (total, _) = self.total_memory();
        if force || total.abs_diff(self.shown_total.get()) >= 1024 * 1024 {
            self.shown_total.set(total);
            log_err(
                self.chrome.evaluate_script(&format!("window.browser && window.browser.setMemoryTotal({total})")),
                "update memory button",
            );
        }
    }

    /// Asks the engine which process draws which page (answer arrives later).
    fn request_renderers(&mut self) {
        if !self.renderer_query_pending {
            match webview_hooks::query_renderers(&self.chrome, self.proxy.clone()) {
                Ok(()) => self.renderer_query_pending = true,
                Err(e) => eprintln!("ask engine for processes: {e}"),
            }
        }
    }

    /// Tabs whose pages are on screen right now.
    fn on_screen_tabs(&self) -> HashSet<TabId> {
        let mut ids: HashSet<TabId> = self.active.into_iter().collect();
        ids.extend(self.fullscreen_tab);
        if let Some((l, r)) = self.split_active_pair() {
            ids.extend([l, r]);
        }
        ids
    }

    /// A tab the user may put to sleep from the memory manager: loaded, not
    /// on screen, and not downloading (that would cancel the download).
    fn can_sleep(&self, tab: &Tab) -> bool {
        tab.webview.is_some()
            && !self.on_screen_tabs().contains(&tab.id)
            && !self.download_tabs.values().any(|&t| t == tab.id)
    }

    /// "Free up memory" also leaves alone tabs playing sound and private
    /// tabs (the last one going to sleep would end their private sign-ins).
    fn can_bulk_sleep(&self, tab: &Tab) -> bool {
        self.can_sleep(tab) && !tab.audible && !tab.private
    }

    /// Puts `ids` to sleep now and says roughly how much memory that freed.
    fn sleep_tabs(&mut self, ids: &[TabId]) {
        // Measure first: once a page closes its process is gone.
        let pages: Vec<u32> = ids
            .iter()
            .filter_map(|&id| self.tab(id))
            .filter_map(|t| t.webview.as_ref())
            .filter_map(webview_hooks::main_frame_id)
            .collect();
        let freed: u64 = memory::processes_freed_by(&self.renderers, &pages)
            .iter()
            .filter_map(|&pid| memory::process_memory(pid))
            .sum();
        for &id in ids {
            self.put_to_sleep(id);
        }
        self.sync();
        let tabs = if ids.len() == 1 { "1 tab".to_string() } else { format!("{} tabs", ids.len()) };
        let text = if freed >= 1024 * 1024 {
            format!("Put {tabs} to sleep · freed about {}", format_bytes(freed))
        } else {
            format!("Put {tabs} to sleep")
        };
        self.show_notice(&text, false);
        self.refresh_memory_panel();
    }

    /// Opens (or, if open, closes) the memory manager under the memory button.
    fn show_memory_panel(&mut self, x: f64, y: f64) {
        if (self.menu.is_visible() && self.menu.kind() == Some(MenuKind::Memory)) || self.menu.recently_closed() {
            self.hide_menu();
            return;
        }
        self.request_renderers();
        let view = self.memory_panel_view();
        let Ok(json) = serde_json::to_string(&view) else { return };
        let rows = view.tabs.len();
        let (sx, sy) = self.to_screen(x, y);
        self.menu.show_memory(&self.window, sx, sy, rows, &json);
    }

    fn refresh_memory_panel(&self) {
        if self.menu.is_visible() && self.menu.kind() == Some(MenuKind::Memory) {
            if let Ok(json) = serde_json::to_string(&self.memory_panel_view()) {
                self.menu.update_memory(&json);
            }
        }
    }

    /// Builds the memory manager's contents: the total, and every tab with
    /// its memory use, biggest first.
    fn memory_panel_view(&self) -> MemoryPanelView<'_> {
        let (total, processes) = self.total_memory();
        let on_screen = self.on_screen_tabs();

        // Each loaded tab's page id, once.
        let pages: Vec<(TabId, u32)> =
            self.tabs.iter().filter_map(|t| Some((t.id, webview_hooks::main_frame_id(t.webview.as_ref()?)?))).collect();
        let all_pages: Vec<u32> = pages.iter().map(|&(_, p)| p).collect();
        let mut process_bytes: HashMap<u32, u64> = HashMap::new();
        let mut bytes_of =
            |pid: u32| *process_bytes.entry(pid).or_insert_with(|| memory::process_memory(pid).unwrap_or(0));

        let mut tabs_pids: HashSet<u32> = HashSet::new();
        let mut rows = Vec::with_capacity(self.tabs.len());
        for tab in &self.tabs {
            let internal = is_downloads_url(&tab.url);
            let page = pages.iter().find(|&&(id, _)| id == tab.id).map(|&(_, p)| p);
            let (memory, shared) = match page {
                Some(page) => {
                    let (pids, shared) = memory::processes_for_page(&self.renderers, page, &all_pages);
                    tabs_pids.extend(&pids);
                    let bytes = (!pids.is_empty()).then(|| pids.iter().map(|&p| bytes_of(p)).sum());
                    (bytes, shared)
                }
                None if tab.webview.is_none() && !internal => (Some(0), 0),
                None => (None, 0),
            };
            let state = if internal {
                "internal"
            } else if tab.webview.is_none() {
                "sleeping"
            } else if on_screen.contains(&tab.id) {
                "visible"
            } else if tab.frozen.get() {
                "frozen"
            } else {
                "awake"
            };
            let site = if internal {
                "Rust Browser".to_string()
            } else if tab.url == NEW_TAB_URL || tab.url.is_empty() {
                "New Tab".to_string()
            } else {
                site_label(&tab.url)
            };
            rows.push(MemoryTabView {
                id: tab.id,
                title: &tab.title,
                site,
                state,
                memory,
                shared,
                private: tab.private,
                audible: tab.audible,
                can_sleep: self.can_sleep(tab),
            });
        }
        // Biggest first; sleeping and unmeasured tabs last.
        rows.sort_by_key(|r| std::cmp::Reverse(r.memory.unwrap_or(0)));
        let tabs_total = tabs_pids.iter().map(|&p| bytes_of(p)).sum();
        let can_free = self.tabs.iter().filter(|t| self.can_bulk_sleep(t)).count();
        MemoryPanelView { total, tabs_total, processes, saver: self.settings.memory_saver, can_free, tabs: rows }
    }

    /// Unloads a tab's page, freeing all of its memory. The tab keeps its
    /// title, icon and address and loads again when it's opened.
    fn put_to_sleep(&mut self, id: TabId) {
        let Some(tab) = self.tab_mut(id) else { return };
        let Some(webview) = tab.webview.take() else { return };
        drop(webview); // closes the page and its engine process (if not shared)
        tab.reset_webview_state();
        tab.loading = false;
        tab.audible = false;
        // The page's back/forward list goes with it.
        tab.can_go_back = false;
        tab.can_go_forward = false;
        if self.menu.card_tab() == Some(id) {
            self.refresh_card();
        }
    }

    /// Memory used by a tab's page right now: (bytes, processes, tabs sharing them).
    fn tab_memory(&self, tab: &Tab) -> (Option<u64>, usize, usize) {
        let Some(wv) = &tab.webview else { return (Some(0), 0, 0) };
        let Some(page) = webview_hooks::main_frame_id(wv) else { return (None, 0, 0) };
        let others: Vec<u32> = self
            .tabs
            .iter()
            .filter(|t| t.id != tab.id)
            .filter_map(|t| t.webview.as_ref())
            .filter_map(webview_hooks::main_frame_id)
            .collect();
        let (pids, shared) = memory::processes_for_page(&self.renderers, page, &others);
        if pids.is_empty() {
            return (None, 0, 0);
        }
        let total = pids.iter().filter_map(|&pid| memory::process_memory(pid)).sum();
        (Some(total), pids.len(), shared)
    }

    /// Development builds only: with `RUST_BROWSER_MEMORY_PROBE` set, ask the
    /// engine every few seconds which process draws which tab ...
    #[cfg(debug_assertions)]
    fn debug_memory_probe(&mut self) {
        // `RUST_BROWSER_HOVER_PROBE=1`: act as if the mouse rests on the
        // first tab, to check the hover card window without real input.
        if std::env::var_os("RUST_BROWSER_HOVER_PROBE").is_some() {
            if let Some(id) = self.tabs.first().map(|t| t.id) {
                self.hover = Some((id, 12.0, 38.0));
                self.refresh_card();
                eprintln!("HOVER card_tab={:?}", self.menu.card_tab());
            }
        }
        // `RUST_BROWSER_PANEL_PROBE=1`: open the memory manager after 20 s,
        // press "Free up memory" after 35 s, and report the totals.
        if std::env::var_os("RUST_BROWSER_PANEL_PROBE").is_some() {
            use std::sync::atomic::{AtomicU8, Ordering};
            static STEP: AtomicU8 = AtomicU8::new(0);
            let elapsed = self.started_at.elapsed().as_secs();
            let step = STEP.load(Ordering::Relaxed);
            if step == 0 && elapsed >= 20 {
                STEP.store(1, Ordering::Relaxed);
                let x = self.window.inner_size().to_logical::<f64>(self.window.scale_factor()).width - 90.0;
                self.show_memory_panel(x, 84.0);
                eprintln!("PANEL opened visible={}", self.menu.is_visible());
            } else if step == 1 && elapsed >= 35 {
                STEP.store(2, Ordering::Relaxed);
                eprintln!("PANEL before total={}", self.total_memory().0);
                self.handle_ui(UiMsg::SleepBackgroundTabs);
                eprintln!("PANEL notice={}", self.notice.as_ref().map_or("", |n| n.text.as_str()));
                eprintln!("PANEL still open right after: {}", self.menu.is_visible());
            } else if step == 2 && elapsed >= 50 {
                STEP.store(3, Ordering::Relaxed);
                let sleeping = self.tabs.iter().filter(|t| t.webview.is_none()).count();
                eprintln!(
                    "PANEL after total={} sleeping_tabs={sleeping} of {}",
                    self.total_memory().0,
                    self.tabs.len()
                );
                eprintln!("PANEL still open 15 s later: {}", self.menu.is_visible());
            }
        }
        if std::env::var_os("RUST_BROWSER_MEMORY_PROBE").is_some()
            && !self.renderer_query_pending
            && webview_hooks::query_renderers(&self.chrome, self.proxy.clone()).is_ok()
        {
            self.renderer_query_pending = true;
        }
    }

    /// ... and print what each tab's hover card would say (to stderr).
    #[cfg(debug_assertions)]
    fn debug_memory_report(&self) {
        if std::env::var_os("RUST_BROWSER_MEMORY_PROBE").is_none() {
            return;
        }
        for tab in &self.tabs {
            let (memory, processes, shared) = self.tab_memory(tab);
            let state = if tab.webview.is_none() {
                "sleeping"
            } else if tab.frozen.get() {
                "frozen"
            } else {
                "awake"
            };
            eprintln!(
                "MEMORY tab={} state={state} bytes={} processes={processes} shared={shared} site={}",
                tab.id,
                memory.map_or("unknown".to_string(), |b| b.to_string()),
                site_label(&tab.url)
            );
        }
        eprintln!("MEMORY end renderers={}", self.renderers.len());
    }

    /// Shows or updates the hover card of the tab under the mouse.
    fn refresh_card(&mut self) {
        let Some((id, x, y)) = self.hover else { return };
        let Some(tab) = self.tab(id) else {
            self.hover = None;
            self.menu.hide_card();
            return;
        };
        let internal = is_downloads_url(&tab.url);
        let (memory, processes, shared) = if internal { (None, 0, 0) } else { self.tab_memory(tab) };
        let state = if internal {
            "internal"
        } else if tab.webview.is_none() {
            "sleeping"
        } else if tab.frozen.get() {
            "frozen"
        } else {
            "awake"
        };
        let site = if internal {
            "Rust Browser".to_string()
        } else if tab.url == NEW_TAB_URL || tab.url.is_empty() {
            "New Tab".to_string()
        } else {
            site_label(&tab.url)
        };
        let title = if tab.title.is_empty() { site.as_str() } else { tab.title.as_str() };
        let card = CardView { title, site: site.clone(), private: tab.private, state, memory, processes, shared };
        let Ok(json) = serde_json::to_string(&card) else { return };
        let (sx, sy) = self.to_screen(x, y);
        if self.menu.card_tab() == Some(id) {
            self.menu.update_card(&json);
        } else {
            self.menu.show_card(&self.window, id, sx, sy, &json);
        }
    }

    fn toggle_mute(&mut self, id: TabId) {
        let Some(tab) = self.tab(id) else { return };
        let Some(wv) = &tab.webview else { return };
        // The muted-changed event updates the tab's icon once WebView2 has done it.
        log_err(webview_hooks::set_muted(wv, !tab.muted), "mute tab");
    }

    fn run_notice_action(&mut self, action: NoticeAction) {
        match action {
            NoticeAction::OpenUrl { url, private } => self.open_tab_logged(&url, Placement::AfterActive, private),
            NoticeAction::Reload(id) => {
                if let Some(wv) = self.tab(id).and_then(|t| t.webview.as_ref()) {
                    log_err(wv.reload(), "reload crashed tab");
                }
            }
            NoticeAction::InstallUpdate => self.install_update(),
            NoticeAction::ShowFile(path) => {
                spawn_explorer(Command::new("explorer.exe").raw_arg(format!("/select,\"{}\"", path.display())))
            }
        }
    }

    // ---- tab search -----------------------------------------------------

    /// Ctrl+Shift+A: a list of the open tabs to filter by typing, centred
    /// under the tab strip.
    fn show_tab_search(&mut self) {
        if (self.menu.is_visible() && self.menu.kind() == Some(MenuKind::TabSearch)) || self.menu.recently_closed() {
            self.hide_menu();
            return;
        }
        if self.is_fullscreen() {
            self.exit_fullscreen();
        }
        let view = TabSearchView {
            tabs: self
                .tabs
                .iter()
                .map(|t| TabSearchRow {
                    id: t.id,
                    title: &t.title,
                    site: if t.url == NEW_TAB_URL || t.url.is_empty() {
                        "New Tab".to_string()
                    } else if is_downloads_url(&t.url) {
                        "Downloads".to_string()
                    } else {
                        site_label(&t.url)
                    },
                    active: Some(t.id) == self.active,
                    private: t.private,
                    audible: t.audible,
                    sleeping: t.webview.is_none() && !is_downloads_url(&t.url),
                })
                .collect(),
        };
        let Ok(json) = serde_json::to_string(&view) else { return };
        let width = f64::from(self.window.inner_size().width) / self.window.scale_factor();
        let (sx, sy) = self.to_screen((width + menu::TAB_SEARCH_WIDTH) / 2.0, 40.0);
        self.menu.show_tab_search(&self.window, sx, sy, view.tabs.len(), &json);
    }

    // ---- screenshots ----------------------------------------------------

    /// Ctrl+Shift+S: saves a picture of the whole active page (PNG) in the
    /// downloads folder.
    fn take_screenshot(&mut self) {
        let Some(id) = self.active else { return };
        let Some(tab) = self.tab(id) else { return };
        if !is_web_page(&tab.url) {
            return;
        }
        let Some(webview) = &tab.webview else { return };
        let scale = self.window.scale_factor() * f64::from(tab.zoom_percent) / 100.0;
        match webview_hooks::capture_full_page(webview, id, scale, self.proxy.clone()) {
            Ok(()) => self.show_notice("Taking a screenshot…", false),
            Err(e) => self.show_notice(&format!("Couldn't take a screenshot: {e}"), true),
        }
    }

    fn save_screenshot(&mut self, id: TabId, image: Vec<u8>) {
        // The site's name without "www." or a port number.
        let site = self.tab(id).and_then(|t| settings::zoom_site(&t.url)).unwrap_or_default();
        let folder = self.settings.download_dir.clone().filter(|d| d.is_dir()).or_else(downloads_folder);
        let Some(folder) = folder else {
            self.show_notice("Couldn't find a folder to save the screenshot in", true);
            return;
        };
        let path = unique_path(&folder, &screenshot_file_name(&site, &local_timestamp()));
        match std::fs::write(&path, image) {
            Ok(()) => {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                self.show_notice_with_action(
                    &format!("Screenshot saved: {name}"),
                    "Show",
                    NoticeAction::ShowFile(path),
                );
            }
            Err(e) => self.show_notice(&format!("Couldn't save the screenshot: {e}"), true),
        }
    }

    // ---- reader mode ----------------------------------------------------

    /// Opens (or closes) reader mode on the active page: its article as
    /// clean, readable text (see `ui/reader.js`).
    fn toggle_reader(&mut self) {
        let Some(id) = self.active else { return };
        let Some(tab) = self.tab(id) else { return };
        if !is_web_page(&tab.url) {
            return;
        }
        let Some(webview) = &tab.webview else { return };
        let proxy = self.proxy.clone();
        log_err(
            webview.evaluate_script_with_callback(READER_JS, move |result| {
                // The script's return value, as JSON ("\"opened\"").
                let result = serde_json::from_str::<String>(&result).unwrap_or_default();
                let _ = proxy.send_event(UserEvent::ReaderResult { id, result });
            }),
            "reader mode",
        );
    }

    // ---- import ---------------------------------------------------------

    /// The menu's "Import from Chrome or Edge…": opens Settings at Import.
    fn import_from_other_browsers(&mut self) {
        if self.panel != Some(Panel::Settings) {
            self.set_panel(Some(Panel::Settings));
        }
        self.flush_sync();
        log_err(
            self.chrome.evaluate_script("window.browser && window.browser.showSettingsSection('import')"),
            "show import settings",
        );
    }

    /// Reads `source`'s bookmarks and history in the background.
    fn import_from(&mut self, source: import::Source) {
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let result = import::import(source);
            let _ = proxy.send_event(UserEvent::Imported { source, result });
        });
        self.show_notice(&format!("Importing from {}…", source.name()), false);
    }

    fn finish_import(&mut self, source: import::Source, result: Result<import::Imported, String>) {
        let imported = match result {
            Ok(imported) => imported,
            Err(e) => {
                self.show_notice(&format!("Import failed: {e}"), true);
                return;
            }
        };
        let bookmarks = self.bookmarks.import(imported.bookmarks);
        let history = self.history.import(imported.history);
        if bookmarks > 0 {
            self.bookmarks_rev += 1;
            self.save_bookmarks();
        }
        if history > 0 {
            self.save_history();
        }
        if bookmarks + history > 0 {
            self.sync_newtab_state();
        }
        let plural = |n: usize, one: &str, many: &str| if n == 1 { format!("1 {one}") } else { format!("{n} {many}") };
        let text = if bookmarks + history == 0 {
            format!("Nothing new to import from {}", source.name())
        } else {
            format!(
                "Imported {} and {} from {}",
                plural(bookmarks, "bookmark", "bookmarks"),
                plural(history, "history entry", "history entries"),
                source.name()
            )
        };
        self.show_notice(&text, false);
    }

    // ---- updates --------------------------------------------------------

    /// The daily automatic check (if it's switched on and due).
    fn check_for_updates_if_due(&mut self) {
        let busy = matches!(
            self.update,
            UpdateStatus::Checking | UpdateStatus::Available | UpdateStatus::Downloading(_) | UpdateStatus::Installing
        );
        if self.settings.check_for_updates && !busy && Instant::now() >= self.next_update_check {
            self.check_for_updates(false);
        }
    }

    /// Asks GitHub for the newest release in the background; the answer comes
    /// back as [`UpdateEvent::Checked`].
    fn check_for_updates(&mut self, manual: bool) {
        if matches!(self.update, UpdateStatus::Checking | UpdateStatus::Downloading(_) | UpdateStatus::Installing) {
            return;
        }
        self.update = UpdateStatus::Checking;
        // Pushed back now, so a slow answer doesn't start a second check.
        self.next_update_check = Instant::now() + UPDATE_CHECK_INTERVAL;
        updater::check(self.proxy.clone(), manual);
        self.sync();
    }

    fn on_update_event(&mut self, event: UpdateEvent) {
        match event {
            UpdateEvent::Checked { result, manual } => {
                // An Update click may have started a download meanwhile.
                if self.update != UpdateStatus::Checking {
                    return;
                }
                match result {
                    Ok(Some(release)) => {
                        let version = release.version;
                        self.update_release = Some(release);
                        self.update = UpdateStatus::Available;
                        if !manual {
                            self.show_notice_with_action(
                                &format!("Rust Browser {version} is available"),
                                "Update",
                                NoticeAction::InstallUpdate,
                            );
                        }
                    }
                    Ok(None) => {
                        self.update_release = None;
                        self.update = UpdateStatus::UpToDate;
                    }
                    Err(e) => {
                        self.next_update_check = Instant::now() + UPDATE_RETRY_INTERVAL;
                        // The daily check fails quietly (e.g. offline); Settings shows why.
                        self.update = UpdateStatus::Failed(format!("Couldn't check for updates: {e}"));
                    }
                }
                self.sync();
            }
            UpdateEvent::Progress(percent) => {
                if matches!(self.update, UpdateStatus::Downloading(_)) {
                    self.update = UpdateStatus::Downloading(Some(percent));
                    self.sync();
                }
            }
            UpdateEvent::Downloaded(Ok(installer)) => {
                if !matches!(self.update, UpdateStatus::Downloading(_)) {
                    return;
                }
                if self.downloads.active_count() > 0 {
                    // Closing now would cancel the user's downloads.
                    self.update = UpdateStatus::Available;
                    self.show_notice("Wait for your downloads to finish, then click Update again", true);
                    return;
                }
                self.update = UpdateStatus::Installing;
                self.flush_sync();
                self.save_session();
                match updater::run_installer(&installer) {
                    // The installer updates the files once this window is gone,
                    // then starts the browser again with the same tabs.
                    Ok(()) => self.exit_requested = true,
                    Err(e) => {
                        let message = format!("Couldn't start the installer: {e}");
                        self.update = UpdateStatus::Failed(message.clone());
                        self.show_notice(&message, true);
                    }
                }
            }
            UpdateEvent::Downloaded(Err(e)) => {
                let message = format!("Update failed: {e}");
                self.update = UpdateStatus::Failed(message.clone());
                self.show_notice(&message, true);
            }
        }
    }

    /// The Update button: download, verify and install the newer version.
    fn install_update(&mut self) {
        if matches!(self.update, UpdateStatus::Checking | UpdateStatus::Downloading(_) | UpdateStatus::Installing) {
            return;
        }
        let Some(release) = self.update_release.clone() else {
            self.check_for_updates(true);
            return;
        };
        if !self.can_update_itself {
            // A portable copy or a build folder: the installer would install a
            // second copy elsewhere, so show the download page instead.
            self.open_tab_logged(&release.page_url, Placement::AfterActive, false);
            return;
        }
        if self.downloads.active_count() > 0 {
            self.show_notice("Wait for your downloads to finish, then click Update again", true);
            return;
        }
        self.update = UpdateStatus::Downloading(None);
        updater::download(release, self.proxy.clone());
        self.sync();
    }

    fn update_view(&self) -> UpdateView<'_> {
        let (state, error) = match &self.update {
            UpdateStatus::Idle => ("idle", None),
            UpdateStatus::Checking => ("checking", None),
            UpdateStatus::UpToDate => ("upToDate", None),
            UpdateStatus::Available => ("available", None),
            UpdateStatus::Downloading(_) => ("downloading", None),
            UpdateStatus::Installing => ("installing", None),
            UpdateStatus::Failed(e) => ("failed", Some(e.as_str())),
        };
        UpdateView {
            state,
            version: self.update_release.as_ref().map(|r| r.version.to_string()),
            percent: match self.update {
                UpdateStatus::Downloading(p) => p,
                _ => None,
            },
            error,
            can_install: self.can_update_itself,
        }
    }

    fn zoom_active(&mut self, up: bool) {
        let Some(id) = self.active else { return };
        if let Some(tab) = self.tab_mut(id) {
            let next = settings::zoom_step(tab.zoom_percent, up);
            if let Some(wv) = &tab.webview {
                tab.zoom_percent = next;
                log_err(wv.zoom(f64::from(next) / 100.0), "zoom");
            }
        }
        self.remember_zoom_of(id);
        self.sync();
    }

    /// Saves tab `id`'s current zoom as its site's zoom (not for private tabs).
    fn remember_zoom_of(&mut self, id: TabId) {
        let Some(tab) = self.tab(id) else { return };
        if tab.private || tab.webview.is_none() {
            return;
        }
        let Some(site) = settings::zoom_site(&tab.url) else { return };
        let zoom = tab.zoom_percent;
        if self.settings.remember_site_zoom(&site, zoom) {
            self.save_settings_quietly();
        }
    }

    // ---- crashes --------------------------------------------------------

    fn process_failed(&mut self, id: TabId, failure: ProcessFailure) {
        match failure {
            ProcessFailure::EngineExited => self.restart_after_engine_crash(),
            ProcessFailure::PageCrashed | ProcessFailure::PageUnresponsive => {
                let Some(tab) = self.tab_mut(id) else { return };
                tab.loading = false;
                tab.audible = false;
                if self.active == Some(id) {
                    let text = if failure == ProcessFailure::PageCrashed {
                        "This page crashed"
                    } else {
                        "This page isn't responding"
                    };
                    self.show_notice_with_action(text, "Reload", NoticeAction::Reload(id));
                } else {
                    self.sync();
                }
            }
            ProcessFailure::Other => {}
        }
    }

    /// The WebView2 engine itself stopped, taking every tab (and this UI)
    /// with it. Start a fresh copy of the browser that reopens the tabs.
    fn restart_after_engine_crash(&mut self) {
        if self.restarting {
            return; // every tab reports the same crash
        }
        self.restarting = true;
        self.save_session();

        // If the engine already crashed right after the last restart, don't
        // loop: tell the user instead.
        let crashing_repeatedly = self.restored_after_crash && self.started_at.elapsed() < Duration::from_secs(30);
        let restarted = !crashing_repeatedly
            && std::env::current_exe().and_then(|exe| Command::new(exe).arg(crate::RESTORE_ARG).spawn()).is_ok();
        if !restarted {
            crate::fatal(
                "The web engine (Microsoft Edge WebView2) stopped unexpectedly.\n\n\
                 Your tabs have been saved. Restart Rust Browser to reopen them. If this keeps happening, \
                 repair or reinstall the WebView2 Runtime from Windows Settings > Apps.",
            );
        }
        self.exit_requested = true;
    }

    // ---- settings -------------------------------------------------------

    fn update_settings(&mut self, patch: SettingsPatch) {
        let before = self.settings.clone();
        if self.settings.apply(patch) {
            self.settings_changed(&before);
        } else {
            self.sync(); // puts the UI back if it showed a rejected value
        }
        // The memory manager shows the memory saver setting too.
        self.refresh_memory_panel();
    }

    /// Call after `self.settings` changed: saves it and makes every setting
    /// that differs from `before` take effect straight away.
    fn settings_changed(&mut self, before: &Settings) {
        if let Some(path) = &self.settings_path {
            if let Err(e) = self.settings.save(path) {
                self.show_notice(&format!("Couldn't save settings: {e}"), true);
            }
        }
        if self.settings.theme != before.theme {
            self.window.set_theme(window_theme(self.settings.theme));
            self.menu.set_theme(window_theme(self.settings.theme), webview_theme(self.settings.theme));
            // The theme belongs to a WebView2 profile. The UI and normal tabs
            // share one; private tabs have their own, so set it there too.
            let theme = webview_theme(self.settings.theme);
            log_err(self.chrome.set_theme(theme), "set theme");
            for tab in self.tabs.iter().chain(&self.background).filter(|t| t.private) {
                if let Some(wv) = &tab.webview {
                    log_err(wv.set_theme(theme), "set theme");
                }
            }
        }
        self.block_popups.set(self.settings.block_popups);
        self.block_ads.set(self.settings.block_ads);
        let privacy = |s: &Settings| (s.save_passwords, s.autofill, s.tracking_prevention, s.safe_browsing);
        let privacy_changed = privacy(&self.settings) != privacy(before);
        if privacy_changed {
            for tab in self.tabs.iter().chain(&self.background) {
                if let Some(wv) = &tab.webview {
                    self.apply_privacy(wv);
                }
            }
        }
        if self.settings.zoom_percent != before.zoom_percent || self.settings.site_zoom != before.site_zoom {
            // Every page takes the new normal zoom, except sites with their own.
            for tab in self.tabs.iter_mut().chain(&mut self.background) {
                let site = if tab.private { None } else { settings::zoom_site(&tab.url) };
                let zoom = self.settings.zoom_for(site.as_deref());
                if tab.zoom_percent != zoom {
                    tab.zoom_percent = zoom;
                    if let Some(wv) = &tab.webview {
                        log_err(wv.zoom(f64::from(zoom) / 100.0), "set zoom");
                    }
                }
            }
        }
        if self.settings.download_dir != before.download_dir {
            self.download_hook.set_folder(self.settings.download_dir.clone());
        }
        if self.settings.show_bookmarks_bar != before.show_bookmarks_bar {
            self.layout();
        }
        if self.settings.search_engine != before.search_engine
            || self.settings.efficiency_mode != before.efficiency_mode
        {
            self.sync_newtab_state();
        }
        self.sync();
    }

    fn choose_download_folder(&mut self) {
        // A native folder picker, modal to the browser window.
        self.flush_sync();
        let mut dialog = rfd::FileDialog::new().set_title("Choose where downloads are saved").set_parent(&self.window);
        if let Some(current) = self.settings.download_dir.as_ref().filter(|dir| dir.is_dir()) {
            dialog = dialog.set_directory(current);
        }
        if let Some(folder) = dialog.pick_folder() {
            self.set_download_folder(Some(folder));
        }
    }

    fn set_download_folder(&mut self, folder: Option<PathBuf>) {
        let before = self.settings.clone();
        self.settings.download_dir = folder;
        if self.settings != before {
            self.settings_changed(&before);
        }
    }

    fn open_downloads_folder(&self) {
        match self.settings.download_dir.as_ref().filter(|dir| dir.is_dir()) {
            Some(dir) => spawn_explorer(Command::new("explorer.exe").arg(dir)),
            None => spawn_explorer(Command::new("explorer.exe").arg("shell:Downloads")),
        }
    }

    fn clear_browsing_data(&mut self) {
        // Cookies, cache and site storage for the whole profile.
        match self.chrome.clear_all_browsing_data() {
            Ok(()) => self.show_notice("Browsing data cleared", false),
            Err(e) => self.show_notice(&format!("Couldn't clear browsing data: {e}"), true),
        }
    }

    fn apply_privacy(&self, webview: &WebView) {
        let s = &self.settings;
        log_err(
            webview_hooks::apply_privacy(webview, s.save_passwords, s.autofill, s.tracking_prevention, s.safe_browsing),
            "apply privacy settings",
        );
    }

    fn show_notice(&mut self, text: &str, error: bool) {
        let id = self.notice.as_ref().map_or(1, |n| n.id.wrapping_add(1));
        self.notice = Some(Notice { id, text: text.to_string(), error, action: None });
        self.notice_action = None;
        self.sync();
    }

    /// A notice with a button, e.g. "Pop-up blocked [Open]".
    fn show_notice_with_action(&mut self, text: &str, label: &'static str, action: NoticeAction) {
        let id = self.notice.as_ref().map_or(1, |n| n.id.wrapping_add(1));
        self.notice = Some(Notice { id, text: text.to_string(), error: false, action: Some(label) });
        self.notice_action = Some(action);
        self.sync();
    }

    fn new_tab_url(&self) -> String {
        match self.settings.new_tab_page {
            NewTabPage::Blank => NEW_TAB_URL.to_string(),
            NewTabPage::HomePage => self.settings.home_url(),
        }
    }

    fn chrome_height(&self) -> f64 {
        chrome_height_for(&self.settings)
    }

    fn split_active_pair(&self) -> Option<(TabId, TabId)> {
        let (l, r) = self.split?;
        if (self.active == Some(l) || self.active == Some(r)) && self.tab(l).is_some() && self.tab(r).is_some() {
            Some((l, r))
        } else {
            None
        }
    }

    fn visible_pane_layout(&self) -> (Option<(TabId, TabId, Rect, Rect)>, Rect) {
        let fullscreen = self.is_fullscreen();
        let full_bounds = self.page_bounds();
        let split_bounds = self.split_active_pair().filter(|_| !fullscreen).map(|pair| {
            let rects = split_content_bounds(&self.window, self.chrome_height());
            (pair.0, pair.1, rects.0, rects.1)
        });
        (split_bounds, full_bounds)
    }

    fn show_active_tabs(&self) {
        let (split_bounds, full_bounds) = self.visible_pane_layout();
        for tab in &self.tabs {
            if tab.webview.is_some() {
                if let Some(b) = pane_bounds_for_tab(split_bounds, full_bounds, self.active, tab.id) {
                    tab.set_webview_low_memory(false);
                    tab.apply_webview_bounds(b);
                    tab.set_webview_visible(true);
                } else {
                    tab.set_webview_visible(false);
                    if !tab.audible {
                        tab.set_webview_low_memory(true);
                    }
                }
            }
        }
    }

    fn stretch_chrome(&self, full: bool) {
        self.stretch_chrome_with_focus(full, true);
    }

    fn stretch_chrome_with_focus(&self, full: bool, focus: bool) {
        let chrome_h = self.chrome_height();
        if full {
            log_err(self.chrome.set_bounds(chrome_bounds(&self.window, true, chrome_h)), "resize UI");
            if focus {
                log_err(self.chrome.focus(), "focus UI");
            }
            for tab in &self.tabs {
                tab.set_webview_visible(false);
            }
        } else {
            self.show_active_tabs();
            log_err(self.chrome.set_bounds(chrome_bounds(&self.window, false, chrome_h)), "resize UI");
        }
    }

    fn active_is_downloads(&self) -> bool {
        self.active_tab().is_some_and(|t| is_downloads_url(&t.url))
    }

    fn chrome_needs_full(&self) -> bool {
        self.panel.is_some() || self.confirm_close || self.active_is_downloads()
    }

    /// Opens, switches or closes a panel. Panels are part of the UI web view,
    /// so opening one stretches that view over the whole window and hides the
    /// active tab; closing it puts everything back.
    fn set_panel(&mut self, panel: Option<Panel>) {
        if panel == Some(Panel::Downloads) {
            self.open_downloads_tab();
            return;
        }
        if self.panel == panel {
            return;
        }
        if panel.is_some() && self.is_fullscreen() {
            self.exit_fullscreen();
        }
        let was_full = self.chrome_needs_full();
        self.panel = panel;
        let now_full = self.chrome_needs_full();

        if was_full != now_full {
            self.stretch_chrome(now_full);
        }
        self.sync();
        if !now_full {
            self.focus_content();
        }
    }

    // ---- fullscreen -----------------------------------------------------

    // Two kinds of full screen share one look (the page fills the screen,
    // no tab strip or toolbar): a page asking for it (e.g. a video's
    // full-screen button) and the user choosing it (F11 or the menu).

    fn is_fullscreen(&self) -> bool {
        self.fullscreen_tab.is_some() || self.window_fullscreen
    }

    /// A page asked to go full screen.
    fn enter_fullscreen(&mut self, id: TabId) {
        if self.fullscreen_tab == Some(id) || self.confirm_close {
            return;
        }
        self.set_panel(None);
        self.fullscreen_tab = Some(id);
        self.apply_fullscreen();
    }

    /// F11: toggles full screen chosen by the user.
    fn toggle_window_fullscreen(&mut self) {
        if self.is_fullscreen() {
            self.exit_fullscreen();
        } else if !self.confirm_close {
            self.set_panel(None);
            self.window_fullscreen = true;
            self.apply_fullscreen();
        }
    }

    /// Leaves both kinds of full screen.
    fn exit_fullscreen(&mut self) {
        if !self.is_fullscreen() {
            return;
        }
        if let Some(id) = self.fullscreen_tab.take() {
            // Tell the page too, or it keeps thinking it is full screen.
            if let Some(wv) = self.tab(id).and_then(|t| t.webview.as_ref()) {
                log_err(
                    wv.evaluate_script("document.fullscreenElement && document.exitFullscreen()"),
                    "exit page fullscreen",
                );
            }
        }
        self.window_fullscreen = false;
        self.apply_fullscreen();
    }

    /// Makes the window, the UI and the page match the full-screen state.
    fn apply_fullscreen(&mut self) {
        let full = self.is_fullscreen();
        let window_is_full = self.window.fullscreen().is_some();
        if full != window_is_full {
            self.window.set_fullscreen(if full { Some(Fullscreen::Borderless(None)) } else { None });
        }
        log_err(self.chrome.set_visible(!full), "show or hide UI for full screen");
        self.layout();
        if full {
            let target = self.fullscreen_tab.or(self.active);
            for tab in &self.tabs {
                if Some(tab.id) != target {
                    tab.set_webview_visible(false);
                }
            }
            if let Some(tab) = target.and_then(|id| self.tab(id)) {
                tab.apply_webview_bounds(self.page_bounds());
                tab.set_webview_visible(true);
            }
        } else if !self.chrome_needs_full() {
            let pair = self.split_active_pair();
            for tab in &self.tabs {
                let show = match pair {
                    Some((l, r)) => tab.id == l || tab.id == r,
                    None => Some(tab.id) == self.active,
                };
                if show {
                    tab.set_webview_visible(true);
                }
            }
        }
        self.focus_content();
        self.sync();
    }

    /// Where pages go: everything below the toolbar, or the whole window
    /// in full screen.
    fn page_bounds(&self) -> Rect {
        if self.is_fullscreen() {
            let size = self.window.inner_size();
            rect(0, size.width, size.height)
        } else {
            content_bounds(&self.window, self.chrome_height())
        }
    }

    // ---- bookmarks & history --------------------------------------------

    fn toggle_bookmarks_bar(&mut self) {
        let before = self.settings.clone();
        self.settings.show_bookmarks_bar = !self.settings.show_bookmarks_bar;
        self.settings_changed(&before);
    }

    fn toggle_active_bookmark(&mut self) {
        let Some(tab) = self.active_tab() else { return };
        if tab.url == NEW_TAB_URL || is_downloads_url(&tab.url) || tab.url.is_empty() {
            return;
        }
        let (url, title) = (tab.url.clone(), tab.title.clone());
        let added = self.bookmarks.toggle(url, title);
        self.bookmarks_rev += 1;
        self.save_bookmarks();
        self.show_notice(if added { "Bookmark added" } else { "Bookmark removed" }, false);
    }

    fn save_bookmarks(&mut self) {
        if let Some(path) = &self.bookmarks_path {
            if let Err(e) = self.bookmarks.save(path) {
                self.show_notice(&format!("Couldn't save bookmarks: {e}"), true);
            }
        }
        self.sync_newtab_state();
    }

    fn save_history(&self) {
        if let Some(path) = &self.history_path {
            if let Err(e) = self.history.save(path) {
                eprintln!("save history to {}: {e}", path.display());
            }
        }
        self.sync_newtab_state();
    }

    /// Updates the shared New Tab snapshot and refreshes any open non-private New Tabs.
    fn sync_newtab_state(&self) {
        let next = NewTabState::from_sources(&self.settings, &self.bookmarks, &self.history, self.efficiency_active());
        let changed = {
            let mut guard = match self.newtab.write() {
                Ok(g) => g,
                Err(poisoned) => poisoned.into_inner(),
            };
            if *guard == next {
                false
            } else {
                *guard = next.clone();
                true
            }
        };
        if !changed {
            return;
        }
        let script = newtab::update_script(&next);
        for tab in &self.tabs {
            if !tab.private && tab.url == NEW_TAB_URL {
                if let Some(wv) = &tab.webview {
                    log_err(wv.evaluate_script(&script), "refresh new tab page");
                }
            }
        }
    }

    fn send_suggestions(&self, query: &str) {
        let mut out: Vec<SuggestionView<'_>> = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();

        for b in self.bookmarks.search(query, 4) {
            if seen.insert(&b.url) {
                out.push(SuggestionView { url: &b.url, title: &b.title, bookmarked: true });
            }
        }
        for h in self.history.search(query, 8) {
            if out.len() >= 8 {
                break;
            }
            if seen.insert(&h.url) {
                out.push(SuggestionView { url: &h.url, title: &h.title, bookmarked: self.bookmarks.contains(&h.url) });
            }
        }

        if let Ok(json) = serde_json::to_string(&out) {
            log_err(
                self.chrome.evaluate_script(&format!("window.browser && window.browser.setSuggestions({json})")),
                "send suggestions",
            );
        }
    }

    // ---- zoom -----------------------------------------------------------

    /// Gives tab `id` the zoom of the site it's showing (or the normal zoom).
    /// Called as pages load, so moving to another site changes the zoom.
    fn apply_site_zoom(&mut self, id: TabId) {
        let Some(tab) = self.tab(id) else { return };
        let site = if tab.private { None } else { settings::zoom_site(&tab.url) };
        // Private tabs keep whatever zoom they're at; they don't remember sites.
        if tab.private && site.is_none() {
            return;
        }
        let zoom = self.settings.zoom_for(site.as_deref());
        if tab.zoom_percent == zoom {
            return;
        }
        if let Some(tab) = self.tab_mut(id) {
            tab.zoom_percent = zoom;
            if let Some(wv) = &tab.webview {
                log_err(wv.zoom(f64::from(zoom) / 100.0), "site zoom");
            }
        }
        if self.active == Some(id) {
            self.sync();
        }
    }

    /// Saves settings changed by browsing (like a site's zoom) without a notice
    /// unless saving fails.
    fn save_settings_quietly(&mut self) {
        if let Some(path) = &self.settings_path {
            if let Err(e) = self.settings.save(path) {
                self.show_notice(&format!("Couldn't save settings: {e}"), true);
            }
        }
    }

    /// Settings > "Reset zoom for all sites".
    fn reset_site_zoom(&mut self) {
        if self.settings.site_zoom.is_empty() {
            return;
        }
        let before = self.settings.clone();
        self.settings.site_zoom.clear();
        self.settings_changed(&before);
        self.show_notice("Every site is back to the normal zoom", false);
    }

    /// Ctrl+0: back to the normal zoom (Settings > Page zoom), which also
    /// forgets the site's own zoom.
    fn reset_active_zoom(&mut self) {
        let Some(id) = self.active else { return };
        let normal = self.settings.zoom_percent;
        if let Some(tab) = self.tab_mut(id) {
            tab.zoom_percent = normal;
            if let Some(wv) = &tab.webview {
                log_err(wv.zoom(f64::from(normal) / 100.0), "reset zoom");
            }
        }
        self.remember_zoom_of(id);
        self.sync();
    }

    // ---- downloads ------------------------------------------------------

    fn download_action(&mut self, id: DownloadId, action: DownloadAction) {
        let Some(download) = self.downloads.get(id) else { return };
        let (state, path) = (download.state, download.path.clone());
        match action {
            DownloadAction::Cancel => {
                if state == DownloadState::InProgress {
                    self.cancelled.insert(id);
                    if !self.download_hook.cancel(id) {
                        // Nothing left to cancel (it ended in the meantime):
                        // settle the entry ourselves so it can't look stuck.
                        self.cancelled.remove(&id);
                        self.downloads.finish(id, DownloadState::Cancelled, None, 0);
                        self.download_tabs.remove(&id);
                        self.release_background_tabs();
                        self.downloads_changed();
                    }
                }
            }
            DownloadAction::Remove => {
                if self.downloads.remove(id) {
                    self.save_downloads();
                }
                self.sync();
            }
            DownloadAction::Open | DownloadAction::Show => {
                if state != DownloadState::Completed || !path.is_file() {
                    self.sync(); // refreshes the "file moved or deleted" note
                } else if action == DownloadAction::Open {
                    spawn_explorer(Command::new("explorer.exe").arg(&path));
                } else {
                    // Explorer wants `/select,"C:\path\file"` as one raw argument.
                    spawn_explorer(Command::new("explorer.exe").raw_arg(format!("/select,\"{}\"", path.display())));
                }
            }
        }
    }

    /// Call after a download starts or ends: saves the list and redraws.
    fn downloads_changed(&mut self) {
        if !self.active_is_downloads() {
            self.downloads_unseen = true;
        }
        self.save_downloads();
        self.sync();
    }

    fn save_downloads(&self) {
        if let Some(path) = &self.downloads_path {
            if let Err(e) = self.downloads.save(path) {
                eprintln!("save downloads to {}: {e}", path.display());
            }
        }
    }

    /// Destroys closed tabs whose downloads have all ended.
    fn release_background_tabs(&mut self) {
        let busy: HashSet<TabId> = self.download_tabs.values().copied().collect();
        self.background.retain(|tab| busy.contains(&tab.id));
    }

    // ---- tabs -----------------------------------------------------------

    /// Handles text entered in the address bar.
    fn navigate(&mut self, input: &str) {
        if let Some(url) = url_input::resolve(input, self.settings.search_engine.search_prefix()) {
            self.load_in_active(url);
        }
    }

    fn open_downloads_tab(&mut self) {
        if self.is_fullscreen() {
            self.exit_fullscreen();
        }
        if let Some(id) = self.tabs.iter().find(|t| is_downloads_url(&t.url)).map(|t| t.id) {
            self.select_tab(id);
        } else {
            let id = self.push_lazy_tab(DOWNLOADS_URL, "Downloads".to_string(), Placement::End, false);
            self.select_tab(id);
        }
    }

    fn load_in_active(&mut self, url: String) {
        let Some(active) = self.active else { return };
        self.set_panel(None);
        if is_downloads_url(&url) {
            if self.tabs.iter().any(|t| t.id != active && is_downloads_url(&t.url)) {
                self.open_downloads_tab();
                return;
            }
            let zoom_default = self.settings.zoom_percent;
            if let Some(tab) = self.tab_mut(active) {
                tab.url = DOWNLOADS_URL.to_string();
                tab.title = "Downloads".to_string();
                tab.favicon.clear();
                tab.loading = false;
                tab.can_go_back = false;
                tab.can_go_forward = false;
                tab.audible = false;
                tab.muted = false;
                tab.zoom_percent = zoom_default;
                // Drop any prior page webview so video/audio/scripts stop immediately.
                tab.webview = None;
                tab.reset_webview_state();
            }
            if self.split.is_some_and(|(l, r)| l == active || r == active) {
                self.split = None;
            }
            self.downloads_unseen = false;
            self.stretch_chrome(true);
            self.sync();
            self.focus_content();
            return;
        }
        let norm_url = newtab::normalize_url(&url);
        let has_view = self.tab(active).is_some_and(|t| t.webview.is_some());
        if !has_view {
            if let Some(tab) = self.tab_mut(active) {
                tab.url = norm_url;
                tab.title.clear();
            }
            if let Err(e) = self.ensure_tab_webview(active) {
                self.show_notice(&format!("Couldn't start page view: {e}"), true);
                return;
            }
        } else if let Some(tab) = self.tab_mut(active) {
            if let Some(wv) = &tab.webview {
                let target_url = newtab::webview_url(&url, tab.private);
                match wv.load_url(target_url) {
                    Ok(()) => {
                        tab.loading = !newtab::is_internal_url(&norm_url);
                        if norm_url == NEW_TAB_URL {
                            tab.title.clear();
                        }
                        tab.url = norm_url;
                    }
                    Err(e) => self.show_notice(&format!("Couldn't open {url}: {e}"), true),
                }
            }
        }
        self.stretch_chrome(self.chrome_needs_full());
        self.sync();
        self.focus_content();
    }

    /// Opens a new tab showing what "New tabs open" in Settings says, with
    /// the address bar ready for typing. A new private tab always starts blank.
    fn new_tab(&mut self, private: bool) {
        let url = if private { NEW_TAB_URL.to_string() } else { self.new_tab_url() };
        self.open_tab_logged(&url, Placement::End, private);
        self.focus_address();
    }

    fn reopen_closed_tab(&mut self) {
        if let Some((url, private)) = self.closed_tabs.pop() {
            if is_downloads_url(&url) {
                self.open_downloads_tab();
            } else {
                self.open_tab_logged(&url, Placement::AfterActive, private);
            }
        }
    }

    fn open_tab_logged(&mut self, url: &str, placement: Placement, private: bool) {
        if let Err(e) = self.open_tab(url, true, placement, private) {
            self.show_notice(&format!("Couldn't open tab: {e}"), true);
        }
    }

    fn push_lazy_tab(&mut self, url: &str, title: String, placement: Placement, private: bool) -> TabId {
        let id = self.next_id;
        self.next_id += 1;
        let tab = Tab {
            id,
            webview: None,
            wv_visible: Cell::new(false),
            low_memory: Cell::new(false),
            applied_bounds: Cell::new(None),
            title,
            url: url.to_string(),
            favicon: String::new(),
            loading: false,
            can_go_back: false,
            can_go_forward: false,
            zoom_percent: self.settings.zoom_percent,
            private,
            audible: false,
            muted: false,
            hidden_since: Cell::new(None),
            frozen: Cell::new(false),
            ads_blocked: Rc::new(Cell::new(0)),
            ads_notified: Rc::new(Cell::new(false)),
        };
        let index = match placement {
            Placement::End => self.tabs.len(),
            Placement::AfterActive => self.active_index().map_or(self.tabs.len(), |i| i + 1),
            Placement::AfterTab(other) => {
                self.tabs.iter().position(|t| t.id == other).map_or(self.tabs.len(), |i| i + 1)
            }
        };
        self.tabs.insert(index, tab);
        id
    }

    fn ensure_tab_webview(&mut self, id: TabId) -> wry::Result<()> {
        let Some((url, zoom_percent, private)) = self.tab(id).and_then(|t| {
            if t.webview.is_none() && !is_downloads_url(&t.url) {
                Some((t.url.clone(), t.zoom_percent, t.private))
            } else {
                None
            }
        }) else {
            return Ok(());
        };

        let bounds = self.page_bounds();
        let (title_proxy, load_proxy, popup_proxy) = (self.proxy.clone(), self.proxy.clone(), self.proxy.clone());
        let gestures: PopupGestures = Rc::new(RefCell::new(VecDeque::new()));
        let popup_gestures = gestures.clone();
        let block_popups = self.block_popups.clone();

        // Note: pages get no IPC handler. They are untrusted and have no way
        // to send messages to the browser.
        let newtab_handler = Arc::clone(&self.newtab);
        let is_private = self.tab(id).is_some_and(|t| t.private);
        let initial_url = newtab::webview_url(&url, is_private).to_string();
        let webview = WebViewBuilder::new_with_web_context(&mut self.web_context)
            .with_environment(self.chrome.environment())
            .with_https_scheme(true)
            .with_custom_protocol(newtab::SCHEME.into(), move |_, req| newtab::handle_request(&newtab_handler, req))
            .with_url(&initial_url)
            .with_bounds(bounds)
            .with_visible(false) // shown by select_tab, so there is never a flash of two tabs
            .with_focused(false)
            // Private tabs get WebView2's InPrivate mode: a separate, throwaway
            // profile for cookies, cache and storage.
            .with_incognito(private)
            .with_theme(webview_theme(self.settings.theme))
            .with_document_title_changed_handler(move |text| {
                let _ = title_proxy.send_event(UserEvent::TitleChanged(id, text));
            })
            .with_on_page_load_handler(move |event, url| {
                let finished = matches!(event, PageLoadEvent::Finished);
                let _ = load_proxy.send_event(UserEvent::PageLoad { id, finished, url });
            })
            .with_new_window_req_handler(move |url, features| {
                // Whether this request came from a click, recorded by the
                // hook in webview_hooks.rs. If that hook isn't there, assume
                // it did rather than break links.
                let clicked = popup_gestures.borrow_mut().pop_front().unwrap_or(true);
                if !clicked && block_popups.get() {
                    let _ = popup_proxy.send_event(UserEvent::PopupBlocked { from: id, url });
                    NewWindowResponse::Deny
                } else if features.position.is_some() || features.size.is_some() {
                    // Sized/positioned popups (e.g. sign-in windows) keep
                    // `window.opener`, which their page needs.
                    NewWindowResponse::Allow
                } else {
                    // Regular new-tab links open inside a new browser tab.
                    let _ = popup_proxy.send_event(UserEvent::OpenInNewTab { from: id, url });
                    NewWindowResponse::Deny
                }
            })
            .with_devtools(true)
            .build_as_child(&self.window)?;
        log_err(webview_hooks::configure_page_webview(&webview), "configure page webview");

        if let Err(e) = self.download_hook.attach(&webview, id) {
            eprintln!("watch downloads in tab {id}: {e}");
        }
        if let Err(e) = shortcut_hook::attach(&webview, id, self.proxy.clone()) {
            eprintln!("watch shortcuts in tab {id}: {e}");
        }
        if let Err(e) = webview_hooks::attach(&webview, id, self.proxy.clone(), gestures) {
            eprintln!("attach webview hooks in tab {id}: {e}");
        }
        if let Some((counter, notified)) = self.tab(id).map(|t| (t.ads_blocked.clone(), t.ads_notified.clone())) {
            let proxy = self.proxy.clone();
            if let Err(e) =
                webview_hooks::attach_ad_blocker(&webview, id, self.block_ads.clone(), counter, notified, proxy)
            {
                eprintln!("attach ad blocker in tab {id}: {e}");
            }
        }
        self.apply_privacy(&webview);
        if zoom_percent != 100 {
            log_err(webview.zoom(f64::from(zoom_percent) / 100.0), "set zoom");
        }

        if let Some(tab) = self.tab_mut(id) {
            tab.webview = Some(webview);
            tab.reset_webview_state();
            tab.loading = !newtab::is_internal_url(&tab.url);
        }
        Ok(())
    }

    fn open_tab(&mut self, url: &str, activate: bool, placement: Placement, private: bool) -> wry::Result<TabId> {
        let title = if is_downloads_url(url) { "Downloads".to_string() } else { String::new() };
        let id = self.push_lazy_tab(url, title, placement, private);
        if let Err(e) = self.ensure_tab_webview(id) {
            self.tabs.retain(|t| t.id != id);
            return Err(e);
        }

        if activate {
            self.select_tab(id);
        } else {
            self.sync();
        }
        Ok(id)
    }

    fn toggle_split_view(&mut self) {
        if self.split_active_pair().is_some() {
            self.close_split_view();
            return;
        }
        let Some(left_id) = self.active else { return };
        if self.tab(left_id).is_some_and(|t| is_downloads_url(&t.url)) {
            return;
        }
        if self.is_fullscreen() {
            self.exit_fullscreen();
        }
        let private = self.tab(left_id).is_some_and(|t| t.private);
        let url = if private { NEW_TAB_URL.to_string() } else { self.new_tab_url() };
        match self.open_tab(&url, false, Placement::AfterTab(left_id), private) {
            Ok(right_id) => {
                if self.ensure_tab_webview(left_id).is_ok() {
                    self.split = Some((left_id, right_id));
                }
                self.select_tab(right_id);
            }
            Err(e) => self.show_notice(&format!("Couldn't open split tab: {e}"), true),
        }
    }

    fn split_tab_with_active(&mut self, other_id: TabId) {
        let Some(left_id) = self.active else { return };
        if left_id == other_id {
            return;
        }
        if self.tab(left_id).is_none_or(|t| is_downloads_url(&t.url))
            || self.tab(other_id).is_none_or(|t| is_downloads_url(&t.url))
        {
            return;
        }
        if self.is_fullscreen() {
            self.exit_fullscreen();
        }
        if self.ensure_tab_webview(left_id).is_err() || self.ensure_tab_webview(other_id).is_err() {
            return;
        }
        self.split = Some((left_id, other_id));
        self.panel = None;
        self.layout();
        self.stretch_chrome(self.chrome_needs_full());
        self.focus_content();
        self.sync();
    }

    fn swap_split_view(&mut self) {
        if let Some((l, r)) = self.split {
            self.split = Some((r, l));
            self.layout();
            self.show_active_tabs();
            self.focus_content();
            self.sync();
        }
    }

    fn close_split_view(&mut self) {
        if self.split.take().is_some() {
            self.layout();
            self.show_active_tabs();
            self.focus_content();
            self.sync();
        }
    }

    fn select_tab(&mut self, id: TabId) {
        self.select_tab_with_focus(id, true);
    }

    fn select_tab_with_focus(&mut self, id: TabId, focus: bool) {
        if self.tab(id).is_none() {
            return;
        }
        // A page's own full screen ends when you leave its tab; F11 carries on.
        if self.fullscreen_tab.is_some() && self.fullscreen_tab != Some(id) {
            self.exit_fullscreen();
        }
        if let Err(e) = self.ensure_tab_webview(id) {
            self.show_notice(&format!("Couldn't restore tab: {e}"), true);
            return;
        }
        if self.active != Some(id) {
            self.status_text.clear();
        }
        self.active = Some(id);
        self.panel = None;
        if self.active_is_downloads() {
            self.downloads_unseen = false;
        }
        if let Some((l, r)) = self.split_active_pair() {
            let partner = if id == l { r } else { l };
            if self.ensure_tab_webview(partner).is_err() {
                self.split = None;
            }
        }
        let full = self.chrome_needs_full();
        self.layout();

        // Order matters:
        // 1. show the new tab(s) first, so there is never a blank frame (or stretch
        //    the UI view over the window when showing an internal tab like Downloads);
        // 2. move keyboard focus to it *before* hiding the old tab - hiding a
        //    web view that still has focus drops the focus on the floor, and
        //    the page then stops receiving key presses;
        // 3. hide every other tab.
        let pair = self.split_active_pair();
        for tab in &self.tabs {
            let show = !full
                && match pair {
                    Some((l, r)) => tab.id == l || tab.id == r,
                    None => tab.id == id,
                };
            if show && tab.webview.is_some() {
                tab.set_webview_low_memory(false);
                tab.set_webview_visible(true);
            }
        }
        self.stretch_chrome_with_focus(full, focus);
        if focus && !self.confirm_close {
            self.focus_content();
        }
        for tab in &self.tabs {
            let keep = !full
                && match pair {
                    Some((l, r)) => tab.id == l || tab.id == r,
                    None => tab.id == id,
                };
            if !keep && tab.webview.is_some() {
                tab.set_webview_visible(false);
                if !tab.audible {
                    tab.set_webview_low_memory(true);
                }
            }
        }
        self.sync();
    }

    fn reorder_tab(&mut self, id: TabId, to_index: usize) {
        let Some(from) = self.tabs.iter().position(|t| t.id == id) else { return };
        let target = to_index.min(self.tabs.len().saturating_sub(1));
        if from == target {
            return;
        }
        let tab = self.tabs.remove(from);
        self.tabs.insert(target, tab);
        self.sync();
    }

    fn close_tab(&mut self, id: TabId) {
        let Some(index) = self.tabs.iter().position(|t| t.id == id) else { return };
        if self.menu.card_tab() == Some(id) {
            self.hover = None;
            self.menu.hide_card();
        }
        if self.tabs.len() == 1 {
            // Closing the last tab closes the browser, like Chrome. The tab
            // stays in the saved session so it can be restored next time.
            self.request_close();
            return;
        }
        if self.fullscreen_tab == Some(id) {
            self.exit_fullscreen();
        }
        let active_split = self.split_active_pair();
        let was_active_split = active_split.is_some_and(|(l, r)| l == id || r == id);
        let split_partner = active_split.and_then(|(l, r)| {
            if l == id {
                Some(r)
            } else if r == id {
                Some(l)
            } else {
                None
            }
        });
        if self.split.is_some_and(|(l, r)| l == id || r == id) {
            self.split = None;
        }
        let was_active = self.active == Some(id);
        let closed = self.tabs.remove(index);
        if closed.url != NEW_TAB_URL && !closed.url.is_empty() {
            // Kept in memory only, so private tabs can be reopened too.
            self.closed_tabs.push((closed.url.clone(), closed.private));
            if self.closed_tabs.len() > MAX_CLOSED_TABS {
                self.closed_tabs.remove(0);
            }
        }

        // Move on to another tab first (which also moves keyboard focus),
        // and only then get rid of the closed one.
        if was_active {
            // If closing a split pane, keep focus on its visible partner;
            // otherwise prefer the tab that slid into this position.
            let next = split_partner
                .filter(|partner| self.tab(*partner).is_some())
                .unwrap_or_else(|| self.tabs[index.min(self.tabs.len() - 1)].id);
            self.select_tab(next);
        } else {
            if was_active_split {
                self.layout();
                self.show_active_tabs();
                self.focus_content();
            }
            self.sync();
        }

        if self.download_tabs.values().any(|&tab| tab == id) {
            // A download is still running in this tab: keep it alive, hidden.
            closed.set_webview_visible(false);
            self.background.push(closed);
        } else {
            drop(closed); // destroys the web view
        }
    }

    fn cycle_tab(&mut self, direction: isize) {
        let n = self.tabs.len() as isize;
        let Some(i) = self.active_index() else { return };
        if n < 2 {
            return;
        }
        let next = (i as isize + direction).rem_euclid(n) as usize;
        self.select_tab(self.tabs[next].id);
    }

    fn focus_address(&self) {
        log_err(self.chrome.focus(), "focus UI");
        self.flush_sync();
        log_err(self.chrome.evaluate_script("window.browser && window.browser.focusAddress()"), "focus address bar");
    }

    /// Gives keyboard focus directly to the active page web view (even if blank),
    /// or to the UI when showing a panel or the Downloads tab.
    fn focus_active_webview(&self) {
        if self.chrome_needs_full() {
            log_err(self.chrome.focus(), "focus UI");
            return;
        }
        if let Some(wv) = self.active_webview() {
            log_err(wv.focus(), "focus tab");
        }
    }

    /// Gives keyboard focus to the active tab's page, to the UI if showing a
    /// panel or the Downloads tab, or to the address bar if the tab is blank.
    fn focus_content(&self) {
        if self.chrome_needs_full() {
            log_err(self.chrome.focus(), "focus UI");
            return;
        }
        match self.active_tab() {
            Some(tab) if tab.url != NEW_TAB_URL => {
                if let Some(wv) = &tab.webview {
                    log_err(wv.focus(), "focus tab");
                }
            }
            Some(_) => self.focus_address(),
            None => {}
        }
    }

    fn send_init(&self) {
        let init = InitView {
            engines: SearchEngine::ALL.iter().map(|&e| EngineView { id: e, name: e.name() }).collect(),
            zoom_levels: &ZOOM_LEVELS,
            about: AboutView {
                version: env!("CARGO_PKG_VERSION"),
                engine: &self.engine_version,
                memory: self.device_memory,
                low_end: self.low_end_device,
            },
            import_sources: import::Source::ALL
                .into_iter()
                .filter(|s| s.is_available())
                .map(|s| ImportSourceView { id: s, name: s.name() })
                .collect(),
        };
        match serde_json::to_string(&init) {
            Ok(json) => {
                self.sent_bookmarks_rev.set(None);
                log_err(
                    self.chrome.evaluate_script(&format!("window.browser && window.browser.init({json})")),
                    "init UI",
                );
            }
            Err(e) => eprintln!("serialize init: {e}"),
        }
    }

    /// Pushes the current state to the UI and updates the window title.
    fn sync(&self) {
        self.sync_pending.set(true);
    }

    pub fn flush_sync(&self) {
        if self.sync_pending.replace(false) {
            self.sync_now();
        }
    }

    fn sync_now(&self) {
        self.sync_pending.set(false);
        let downloads_open = self.panel == Some(Panel::Downloads) || self.active_is_downloads();
        let bookmarks_open = self.panel == Some(Panel::Bookmarks);
        let history_open = self.panel == Some(Panel::History);
        let need_bookmarks = bookmarks_open || self.settings.show_bookmarks_bar;
        let bookmarks = if need_bookmarks && self.sent_bookmarks_rev.get() != Some(self.bookmarks_rev) {
            self.sent_bookmarks_rev.set(Some(self.bookmarks_rev));
            Some(self.bookmarks.items())
        } else {
            None
        };
        let bookmarked = self.active_tab().is_some_and(|t| self.bookmarks.contains(&t.url));

        let state = StateView {
            tabs: self
                .tabs
                .iter()
                .map(|t| TabView {
                    id: t.id,
                    title: &t.title,
                    url: &t.url,
                    favicon: &t.favicon,
                    loading: t.loading,
                    can_go_back: t.can_go_back,
                    can_go_forward: t.can_go_forward,
                    zoom_percent: t.zoom_percent,
                    private: t.private,
                    audible: t.audible,
                    muted: t.muted,
                    sleeping: t.webview.is_none() && !is_downloads_url(&t.url),
                    ads_blocked: if self.settings.block_ads { t.ads_blocked.get() } else { 0 },
                    can_read: t.webview.is_some() && is_web_page(&t.url),
                })
                .collect(),
            active: self.active,
            split: self.split.map(|(l, r)| [l, r]),
            maximized: self.maximized,
            panel: self.panel,
            confirm_close: self.confirm_close,
            settings: &self.settings,
            bookmarked,
            status_text: &self.status_text,
            notice: self.notice.as_ref(),
            downloads: if downloads_open {
                self.downloads.items().iter().map(download_view).collect()
            } else {
                Vec::new()
            },
            downloads_active: self.downloads.active_count(),
            downloads_progress: self.downloads.active_progress(),
            downloads_unseen: self.downloads_unseen,
            downloads_added_seq: self.downloads_added_seq,
            downloads_latest: self.downloads.items().first().map(Download::file_name),
            refresh_hz: self.refresh_hz,
            bookmarks,
            history: if history_open { self.history.items() } else { &[] },
            update: self.update_view(),
            efficiency: self.efficiency_active(),
        };
        match serde_json::to_string(&state) {
            Ok(json) => log_err(
                self.chrome.evaluate_script(&format!("window.browser && window.browser.render({json})")),
                "update UI",
            ),
            Err(e) => eprintln!("serialize state: {e}"),
        }

        let title = match self.active_tab() {
            Some(tab) if !tab.title.is_empty() => format!("{} - {APP_NAME}", tab.title),
            _ => APP_NAME.to_string(),
        };
        if *self.last_title.borrow() != title {
            self.window.set_title(&title);
            *self.last_title.borrow_mut() = title;
        }

        self.save_session();
    }

    /// Remembers the open tabs for "continue where you left off". Runs on
    /// every change but only touches the disk when the tab list differs.
    fn save_session(&self) {
        let Some(path) = &self.session_path else { return };
        if let Some(prev) = self.saved_session.borrow().as_ref() {
            let mut count = 0usize;
            let mut active_idx = None;
            let mut same = true;
            for tab in self.tabs.iter().filter(|t| !t.private) {
                if Some(tab.id) == self.active {
                    active_idx = Some(count);
                }
                if prev.tabs.get(count) != Some(&tab.url) || prev.titles.get(count) != Some(&tab.title) {
                    same = false;
                    break;
                }
                count += 1;
            }
            if same && prev.tabs.len() == count && prev.titles.len() == count && prev.active == active_idx.unwrap_or(0)
            {
                return;
            }
        }
        // Private tabs are never written to disk.
        let kept: Vec<&Tab> = self.tabs.iter().filter(|t| !t.private).collect();
        let session = Session {
            tabs: kept.iter().map(|t| t.url.clone()).collect(),
            titles: kept.iter().map(|t| t.title.clone()).collect(),
            active: kept.iter().position(|t| Some(t.id) == self.active).unwrap_or(0),
        };
        let mut saved = self.saved_session.borrow_mut();
        if saved.as_ref() != Some(&session) {
            if let Err(e) = session.save(path) {
                eprintln!("save session to {}: {e}", path.display());
            }
            *saved = Some(session);
        }
    }

    fn tab(&self, id: TabId) -> Option<&Tab> {
        self.tabs.iter().find(|t| t.id == id)
    }

    fn tab_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|t| t.id == id)
    }

    fn active_tab(&self) -> Option<&Tab> {
        self.active.and_then(|id| self.tab(id))
    }

    fn active_webview(&self) -> Option<&WebView> {
        self.active_tab().and_then(|t| t.webview.as_ref())
    }

    fn active_index(&self) -> Option<usize> {
        self.active.and_then(|id| self.tabs.iter().position(|t| t.id == id))
    }
}

fn download_view(download: &Download) -> DownloadView<'_> {
    DownloadView {
        id: download.id,
        name: download.file_name(),
        url: &download.url,
        received: download.received,
        total: download.total,
        state: download.state,
        missing: download.state == DownloadState::Completed && !download.path.is_file(),
    }
}

fn is_downloads_url(url: &str) -> bool {
    url.eq_ignore_ascii_case(DOWNLOADS_URL)
}

/// "84.3 MB", "312 MB", "1.24 GB" (the same style as the UI).
fn format_bytes(bytes: u64) -> String {
    let mb = bytes as f64 / (1024.0 * 1024.0);
    if mb >= 1024.0 {
        format!("{:.2} GB", mb / 1024.0)
    } else if mb >= 100.0 {
        format!("{mb:.0} MB")
    } else {
        format!("{mb:.1} MB")
    }
}

/// A short name for where a page comes from: the host name for web pages
/// ("en.wikipedia.org"), "Local file" for files, else the address itself.
fn site_label(url: &str) -> String {
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("file:") {
        return "Local file".to_string();
    }
    let rest = ["https://", "http://", "view-source:https://", "view-source:http://"]
        .iter()
        .find_map(|scheme| lower.starts_with(scheme).then(|| &url[scheme.len()..]));
    match rest {
        Some(rest) => {
            let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
            // Drop any "user:password@" part; it should never be shown.
            host.rsplit('@').next().unwrap_or(host).to_string()
        }
        None => url.to_string(),
    }
}

fn is_web_url(url: &str) -> bool {
    if newtab::is_internal_url(url) {
        return false;
    }
    let lower = url.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// Runs Explorer to open a file, reveal it, or show a folder.
fn spawn_explorer(command: &mut Command) {
    if let Err(e) = command.spawn() {
        eprintln!("start explorer: {e}");
    }
}

/// Theme for the window frame; `None` follows Windows.
fn window_theme(theme: Theme) -> Option<tao::window::Theme> {
    match theme {
        Theme::System => None,
        Theme::Light => Some(tao::window::Theme::Light),
        Theme::Dark => Some(tao::window::Theme::Dark),
    }
}

/// Theme for the web views: the UI's colours and pages' preferred scheme.
fn webview_theme(theme: Theme) -> wry::Theme {
    match theme {
        Theme::System => wry::Theme::Auto,
        Theme::Light => wry::Theme::Light,
        Theme::Dark => wry::Theme::Dark,
    }
}

fn chrome_height_for(settings: &Settings) -> f64 {
    if settings.show_bookmarks_bar {
        CHROME_HEIGHT + BOOKMARKS_BAR_HEIGHT
    } else {
        CHROME_HEIGHT
    }
}

fn chrome_height_px(window: &Window, height_css: f64) -> u32 {
    let height = (height_css * window.scale_factor()).round() as u32;
    height.min(window.inner_size().height)
}

/// Bounds of the UI web view: a strip at the top, or the whole window while
/// a panel is open (`full`).
fn chrome_bounds(window: &Window, full: bool, height_css: f64) -> Rect {
    let size = window.inner_size();
    let height = if full { size.height } else { chrome_height_px(window, height_css) };
    rect(0, size.width, height)
}

fn content_bounds(window: &Window, height_css: f64) -> Rect {
    let size = window.inner_size();
    let top = chrome_height_px(window, height_css);
    Rect {
        position: PhysicalPosition::new(0, top as i32).into(),
        size: PhysicalSize::new(size.width, size.height.saturating_sub(top)).into(),
    }
}

fn split_content_bounds(window: &Window, height_css: f64) -> (Rect, Rect) {
    let size = window.inner_size();
    let top = chrome_height_px(window, height_css);
    split_bounds_for(size.width, size.height, top, window.scale_factor())
}

fn split_bounds_for(width: u32, height: u32, top: u32, scale_factor: f64) -> (Rect, Rect) {
    let content_h = height.saturating_sub(top);
    let gap = ((4.0 * scale_factor).round().max(2.0) as u32).min(width);
    let left_w = width.saturating_sub(gap) / 2;
    let right_x = left_w + gap;
    let right_w = width.saturating_sub(right_x);
    let left = Rect {
        position: PhysicalPosition::new(0, top as i32).into(),
        size: PhysicalSize::new(left_w, content_h).into(),
    };
    let right = Rect {
        position: PhysicalPosition::new(right_x as i32, top as i32).into(),
        size: PhysicalSize::new(right_w, content_h).into(),
    };
    (left, right)
}

impl Tab {
    fn reset_webview_state(&self) {
        self.wv_visible.set(false);
        self.low_memory.set(false);
        self.applied_bounds.set(None);
        self.hidden_since.set(None);
        self.frozen.set(false);
    }

    fn set_webview_visible(&self, show: bool) {
        if let Some(wv) = &self.webview {
            if self.wv_visible.get() != show {
                log_err(wv.set_visible(show), if show { "show tab" } else { "hide tab" });
                self.wv_visible.set(show);
                // Showing a frozen page wakes it (WebView2 resumes it by itself).
                self.hidden_since.set(if show { None } else { Some(Instant::now()) });
                if show {
                    self.frozen.set(false);
                }
            }
        }
    }

    fn set_webview_low_memory(&self, low: bool) {
        if let Some(wv) = &self.webview {
            if self.low_memory.get() != low {
                let level = if low { MemoryUsageLevel::Low } else { MemoryUsageLevel::Normal };
                let _ = wv.set_memory_usage_level(level);
                self.low_memory.set(low);
            }
        }
    }

    fn apply_webview_bounds(&self, bounds: Rect) {
        let key = bounds_key(&bounds);
        if self.applied_bounds.get() == Some(key) {
            return;
        }
        if let Some(wv) = &self.webview {
            log_err(wv.set_bounds(bounds), "resize tab");
            self.applied_bounds.set(Some(key));
        }
    }
}

fn pane_bounds_for_tab(
    split_bounds: Option<(TabId, TabId, Rect, Rect)>,
    full_bounds: Rect,
    active: Option<TabId>,
    tab_id: TabId,
) -> Option<Rect> {
    match split_bounds {
        Some((l, r, lb, rb)) => {
            if tab_id == l {
                Some(lb)
            } else if tab_id == r {
                Some(rb)
            } else {
                None
            }
        }
        None if active == Some(tab_id) => Some(full_bounds),
        None => None,
    }
}

fn bounds_key(r: &Rect) -> (i32, i32, u32, u32) {
    let pos = r.position.to_physical::<i32>(1.0);
    let size = r.size.to_physical::<u32>(1.0);
    (pos.x, pos.y, size.width, size.height)
}

fn rect(top: i32, width: u32, height: u32) -> Rect {
    Rect { position: PhysicalPosition::new(0, top).into(), size: PhysicalSize::new(width, height).into() }
}

fn log_err<T, E: std::fmt::Display>(result: Result<T, E>, what: &str) {
    if let Err(e) = result {
        eprintln!("{what}: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_bounds_cover_window_width_and_clamp_when_minimized() {
        let (left, right) = split_bounds_for(1281, 800, 88, 1.0);
        let (PhysicalPosition { x: lx, y: ly }, PhysicalSize { width: lw, height: lh }) =
            (left.position.to_physical::<i32>(1.0), left.size.to_physical::<u32>(1.0));
        let (PhysicalPosition { x: rx, y: ry }, PhysicalSize { width: rw, height: rh }) =
            (right.position.to_physical::<i32>(1.0), right.size.to_physical::<u32>(1.0));
        assert_eq!((lx, ly, lh), (0, 88, 712));
        assert_eq!((ry, rh), (88, 712));
        assert_eq!(lw + (rx as u32 - lw) + rw, 1281);

        let (z_left, z_right) = split_bounds_for(0, 0, 0, 1.5);
        assert_eq!(z_right.position.to_physical::<i32>(1.0).x, 0);
        assert_eq!(z_left.size.to_physical::<u32>(1.0).width, 0);
        assert_eq!(z_right.size.to_physical::<u32>(1.0).width, 0);
    }

    #[test]
    fn chrome_height_adds_bookmarks_bar_only_when_enabled() {
        let mut settings = Settings::default();
        assert_eq!(chrome_height_for(&settings), CHROME_HEIGHT);
        settings.show_bookmarks_bar = true;
        assert_eq!(chrome_height_for(&settings), CHROME_HEIGHT + BOOKMARKS_BAR_HEIGHT);
    }

    #[test]
    fn format_bytes_matches_the_ui() {
        assert_eq!(format_bytes(87 * 1024 * 1024 + 400 * 1024), "87.4 MB");
        assert_eq!(format_bytes(412 * 1024 * 1024), "412 MB");
        assert_eq!(format_bytes(1270 * 1024 * 1024), "1.24 GB");
    }

    #[test]
    fn screenshot_names_are_safe_and_unique() {
        assert_eq!(
            screenshot_file_name("example.com", "2026-11-16 14.05.09"),
            "Screenshot example.com 2026-11-16 14.05.09.png"
        );
        assert_eq!(screenshot_file_name("a:b/c?*", "t"), "Screenshot abc t.png");
        assert_eq!(screenshot_file_name("  ", "t"), "Screenshot t.png");
        assert!(local_timestamp().len() == 19 && !local_timestamp().contains(':'));

        let dir = std::env::temp_dir().join(format!("rust-browser-shots-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = unique_path(&dir, "Shot.png");
        assert_eq!(first, dir.join("Shot.png"));
        std::fs::write(&first, b"x").unwrap();
        assert_eq!(unique_path(&dir, "Shot.png"), dir.join("Shot (2).png"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn site_label_shows_only_the_host() {
        assert_eq!(site_label("https://en.wikipedia.org/wiki/Rust"), "en.wikipedia.org");
        assert_eq!(site_label("http://localhost:8080/app?x=1"), "localhost:8080");
        assert_eq!(site_label("HTTPS://Example.com#top"), "Example.com");
        assert_eq!(site_label("https://user:secret@example.com/"), "example.com");
        assert_eq!(site_label("file:///C:/notes.html"), "Local file");
        assert_eq!(site_label("view-source:https://example.com/a"), "example.com");
        assert_eq!(site_label("about:blank"), "about:blank");
    }

    #[test]
    fn pane_bounds_for_tab_routes_single_split_and_hidden_tabs() {
        let full = rect(88, 1280, 712);
        let left = rect(88, 639, 712);
        let right = Rect { position: PhysicalPosition::new(641, 88).into(), size: PhysicalSize::new(639, 712).into() };

        assert_eq!(pane_bounds_for_tab(None, full, Some(1), 1).map(|r| bounds_key(&r)), Some((0, 88, 1280, 712)));
        assert!(pane_bounds_for_tab(None, full, Some(1), 2).is_none());

        let split = Some((1, 2, left, right));
        assert_eq!(pane_bounds_for_tab(split, full, Some(1), 1).map(|r| bounds_key(&r)), Some((0, 88, 639, 712)));
        assert_eq!(pane_bounds_for_tab(split, full, Some(1), 2).map(|r| bounds_key(&r)), Some((641, 88, 639, 712)));
        assert!(pane_bounds_for_tab(split, full, Some(1), 3).is_none());
    }
}
