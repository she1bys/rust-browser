//! Messages into the Rust event loop.
//!
//! The browser UI (tabs, toolbar, panels) talks to Rust through
//! `window.ipc.postMessage(json)`; anything that doesn't parse as a [`UiMsg`]
//! is dropped. Web pages are untrusted and get no message channel at all:
//! everything Rust learns about a page comes from WebView2 itself.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::import::{Imported, Source as ImportSource};
use crate::settings::SettingsPatch;
use crate::updater::Release;

pub type TabId = u32;
pub type DownloadId = u64;

/// A full-window panel shown by the browser UI in place of the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Panel {
    Settings,
    Downloads,
    Bookmarks,
    History,
}

/// What the user clicked on a row in the Downloads panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DownloadAction {
    Open,
    Show,
    Cancel,
    Remove,
}

/// The window has no Windows title bar, so the UI asks for these itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WindowAction {
    Minimize,
    ToggleMaximize,
    Close,
    /// The mouse went down on the empty part of the tab strip: move the window.
    Drag,
    /// The mouse went down on the top edge (or a top corner): resize the window.
    ResizeTop,
    ResizeTopLeft,
    ResizeTopRight,
}

/// Browser commands shared by the main menu, keyboard shortcuts and the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Command {
    NewTab,
    NewPrivateTab,
    ReopenClosedTab,
    ToggleSplitView,
    SwapSplitView,
    ToggleBookmark,
    ToggleBookmarksBar,
    Bookmarks,
    History,
    Downloads,
    Settings,
    ZoomIn,
    ZoomOut,
    ResetZoom,
    /// F11: the page fills the whole screen.
    Fullscreen,
    /// F9: show the page's article as clean, readable text.
    ReaderMode,
    /// Ctrl+Shift+A: find an open tab by typing.
    SearchTabs,
    /// Ctrl+Shift+S: save a picture of the whole page.
    Screenshot,
    /// Bring in bookmarks and history from Chrome or Edge.
    ImportData,
    /// Ctrl+U: the page's HTML source, in a new tab.
    ViewSource,
    /// Ctrl+Shift+Delete: Settings, at "Clear browsing data".
    ClearBrowsingData,
    Find,
    Print,
    SavePage,
    DevTools,
    Exit,
}

/// Actions in a tab's right-click context menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TabCommand {
    NewTabToTheRight,
    /// Pin the tab (a small icon-only tab kept at the left) or unpin it.
    TogglePin,
    SplitWithActive,
    SwapSplitView,
    CloseSplitView,
    Reload,
    Duplicate,
    ToggleMute,
    Close,
    CloseOthers,
    CloseToTheRight,
    ReopenClosed,
}

fn default_true() -> bool {
    true
}

/// Sent by the trusted browser UI (`ui/index.html`).
///
/// `rename_all` covers the message names (`openUrl`); `rename_all_fields`
/// covers the fields inside them (`newTab`). Both are needed: the UI writes
/// everything in camelCase.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum UiMsg {
    /// The UI finished loading and wants the current state.
    Ready,
    Navigate {
        input: String,
    },
    NewTab,
    CloseTab {
        id: TabId,
    },
    SelectTab {
        id: TabId,
        #[serde(default = "default_true")]
        focus: bool,
    },
    Back,
    Forward,
    Reload,
    Stop,
    /// Give keyboard focus back to the page (e.g. after Escape in the address bar).
    FocusContent,
    TogglePanel {
        panel: Panel,
    },
    ClosePanel,
    /// Go to the home page (the Home button).
    GoHome,
    ReorderTab {
        id: TabId,
        to_index: usize,
    },
    ReopenClosedTab,
    ResetZoom,
    ToggleBookmark,
    RemoveBookmark {
        url: String,
    },
    OpenUrl {
        url: String,
        new_tab: bool,
    },
    RemoveHistory {
        url: String,
    },
    ClearHistory,
    QuerySuggestions {
        query: String,
    },
    ConfirmClose,
    CancelClose,
    UpdateSettings {
        patch: SettingsPatch,
    },
    /// Show a folder picker for where downloads are saved.
    ChooseDownloadFolder,
    /// Go back to the Windows Downloads folder.
    ResetDownloadFolder,
    /// Delete cookies, cache and site data.
    ClearBrowsingData,
    ResetSettings,
    DownloadAction {
        id: DownloadId,
        action: DownloadAction,
    },
    ClearDownloads,
    OpenDownloadsFolder,
    Window {
        action: WindowAction,
    },
    Command {
        command: Command,
    },
    MenuCommand {
        command: Command,
        close: bool,
    },
    TabMenuCommand {
        id: TabId,
        command: TabCommand,
    },
    CloseMenu,
    /// Show the main menu. `x`, `y`: bottom-right corner of the menu
    /// button, in UI (CSS) pixels.
    ShowMenu {
        x: f64,
        y: f64,
    },
    /// Right-click on a tab, at UI (CSS) pixel position `x`, `y`.
    TabMenu {
        id: TabId,
        x: f64,
        y: f64,
    },
    ToggleMute {
        id: TabId,
    },
    /// The mouse rests on tab `id`: show its card (with live memory use)
    /// below the point `x`, `y` (CSS pixels). Repeated while it stays there.
    TabHover {
        id: TabId,
        x: f64,
        y: f64,
    },
    /// The mouse left the tab (or the tab was clicked): hide the card.
    TabHoverEnd,
    /// Open (or close) the memory manager under the memory button. `x`, `y`:
    /// bottom-right corner of the button, in CSS pixels.
    ShowMemoryPanel {
        x: f64,
        y: f64,
    },
    /// Sent every second or so while the memory manager is open.
    MemoryPanelRefresh,
    /// Put one tab to sleep now (frees its memory).
    SleepTab {
        id: TabId,
    },
    /// "Free up memory": put every background tab that can sleep to sleep now.
    SleepBackgroundTabs,
    /// The button on the current notice (e.g. "Open" on "Pop-up blocked").
    NoticeAction {
        id: u32,
    },
    /// "Check for updates" in Settings > About.
    CheckForUpdates,
    /// Settings > Appearance: forget every site's own zoom.
    ResetSiteZoom,
    /// Settings > Import: bring in bookmarks and history from another browser.
    ImportFrom {
        source: ImportSource,
    },
    /// The Update button: download the new version and install it.
    InstallUpdate,
    /// Bring in bookmarks from a bookmarks file (.html) another browser exported.
    ImportBookmarksFile,
    /// Save all bookmarks as a bookmarks file other browsers can import.
    ExportBookmarks,
}

/// Progress of an update, reported by the updater's background threads.
#[derive(Debug)]
pub enum UpdateEvent {
    /// The answer to "is there a newer version?": `Some` only if there is.
    /// `manual`: the user asked (rather than the daily check).
    Checked { result: Result<Option<Release>, String>, manual: bool },
    /// Percent of the installer downloaded.
    Progress(u8),
    /// The installer is downloaded and verified (its path), or why not.
    Downloaded(Result<PathBuf, String>),
}

/// Browser shortcuts pressed while a page has focus (see `shortcut_hook.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shortcut {
    NewTab,
    CloseTab,
    ReopenTab,
    FocusAddress,
    NextTab,
    PrevTab,
    SelectTab(usize),
    LastTab,
    /// Open the Downloads panel (Ctrl+J).
    Downloads,
    /// Open or toggle the Settings panel (Ctrl+,).
    Settings,
    /// Open or toggle the History panel (Ctrl+H).
    History,
    /// Toggle bookmark on active page (Ctrl+D).
    Bookmark,
    /// Navigate active tab to home page (Alt+Home).
    Home,
    /// Reset page zoom to 100% (Ctrl+0).
    ResetZoom,
    /// Ctrl+Shift+N.
    NewPrivateTab,
    /// Ctrl+Shift+O.
    Bookmarks,
    /// Ctrl+Shift+B.
    ToggleBookmarksBar,
    /// F11.
    Fullscreen,
    /// F9: reader mode.
    Reader,
    /// Ctrl+Shift+A.
    SearchTabs,
    /// Ctrl+Shift+S.
    Screenshot,
    /// Ctrl+S.
    SavePage,
    /// Ctrl+U.
    ViewSource,
    /// Ctrl+Shift+Delete.
    ClearBrowsingData,
    /// Alt+F or Alt+E: open the main menu.
    Menu,
}

/// One page (renderer) process of the engine and the pages it draws, given
/// by their main frame ids (see `webview_hooks::query_renderers`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RendererInfo {
    pub pid: u32,
    pub pages: Vec<u32>,
}

/// Why a page's engine process stopped (see `webview_hooks.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessFailure {
    /// The page's process crashed or was killed.
    PageCrashed,
    /// The page stopped responding.
    PageUnresponsive,
    /// The whole WebView2 engine stopped: every tab is gone.
    EngineExited,
    /// Something less important (a helper or an iframe's process).
    Other,
}

/// Everything that can wake the event loop besides window events.
#[derive(Debug)]
pub enum UserEvent {
    Ui(UiMsg),
    /// A browser shortcut pressed inside the page of this tab.
    Shortcut(TabId, Shortcut),
    TitleChanged(TabId, String),
    PageLoad {
        id: TabId,
        finished: bool,
        url: String,
    },
    HistoryChanged {
        id: TabId,
        can_go_back: bool,
        can_go_forward: bool,
    },
    FaviconChanged {
        id: TabId,
        url: String,
    },
    StatusTextChanged {
        id: TabId,
        text: String,
    },
    FullScreenChanged {
        id: TabId,
        fullscreen: bool,
    },
    ZoomChanged {
        id: TabId,
        factor: f64,
    },
    /// A page asked for a new window (`target=_blank`, `window.open`).
    OpenInNewTab {
        from: TabId,
        url: String,
    },
    /// A page tried to open a window without being clicked, and it was blocked.
    PopupBlocked {
        from: TabId,
        url: String,
    },
    AudioChanged {
        id: TabId,
        playing: bool,
        muted: bool,
    },
    ProcessFailed {
        id: TabId,
        failure: ProcessFailure,
    },
    /// The engine process behind the browser UI itself (tab strip, toolbar) failed.
    UiProcessFailed(ProcessFailure),
    /// Focus moved into a tab's page view (used to switch active pane in split view).
    TabFocused(TabId),
    /// Focus left the popup menu WebView2 controller.
    MenuLostFocus,
    /// The popup's web view (created on first use) finished loading.
    MenuReady,
    /// The ad blocker blocked more requests in this tab.
    AdsBlocked(TabId),
    /// A picture of tab `id`'s whole page, as PNG (or why not).
    Screenshot {
        id: TabId,
        result: Result<Vec<u8>, String>,
    },
    /// What reader mode did in tab `id`: "opened", "closed" or "none".
    ReaderResult {
        id: TabId,
        result: String,
    },
    /// Opening the find bar through the WebView2 API didn't work.
    FindFailed(TabId),
    /// Every few seconds: lets the memory saver freeze or put tabs to sleep.
    Tick,
    /// The engine's answer to "which process draws which page".
    RendererInfos(Vec<RendererInfo>),
    /// Another `rust-browser.exe` launch forwarded its optional target URL/search.
    ExternalOpen(Option<String>),
    Update(UpdateEvent),
    /// Bookmarks and history read from another browser (or why not).
    Imported {
        source: ImportSource,
        result: Result<Imported, String>,
    },
    DownloadStarted {
        id: DownloadId,
        tab: TabId,
        url: String,
        path: PathBuf,
        total: i64,
    },
    DownloadProgress {
        id: DownloadId,
        received: i64,
        total: i64,
    },
    DownloadFinished {
        id: DownloadId,
        completed: bool,
        path: Option<PathBuf>,
        received: i64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{SearchEngine, Theme};

    #[test]
    fn parses_ui_messages() {
        let m: UiMsg = serde_json::from_str(r#"{"type":"navigate","input":"rust"}"#).unwrap();
        assert_eq!(m, UiMsg::Navigate { input: "rust".into() });
        let m: UiMsg = serde_json::from_str(r#"{"type":"closeTab","id":3}"#).unwrap();
        assert_eq!(m, UiMsg::CloseTab { id: 3 });
        let m: UiMsg = serde_json::from_str(r#"{"type":"ready"}"#).unwrap();
        assert_eq!(m, UiMsg::Ready);
    }

    #[test]
    fn parses_panel_and_settings_messages() {
        let m: UiMsg =
            serde_json::from_str(r#"{"type":"updateSettings","patch":{"searchEngine":"duckduckgo"}}"#).unwrap();
        let expected = SettingsPatch { search_engine: Some(SearchEngine::DuckDuckGo), ..Default::default() };
        assert_eq!(m, UiMsg::UpdateSettings { patch: expected });
        let m: UiMsg =
            serde_json::from_str(r#"{"type":"updateSettings","patch":{"theme":"dark","showHomeButton":true}}"#)
                .unwrap();
        let expected = SettingsPatch { theme: Some(Theme::Dark), show_home_button: Some(true), ..Default::default() };
        assert_eq!(m, UiMsg::UpdateSettings { patch: expected });
        for (json, expected) in [
            (r#"{"type":"goHome"}"#, UiMsg::GoHome),
            (r#"{"type":"chooseDownloadFolder"}"#, UiMsg::ChooseDownloadFolder),
            (r#"{"type":"resetDownloadFolder"}"#, UiMsg::ResetDownloadFolder),
            (r#"{"type":"clearBrowsingData"}"#, UiMsg::ClearBrowsingData),
            (r#"{"type":"resetSettings"}"#, UiMsg::ResetSettings),
        ] {
            assert_eq!(serde_json::from_str::<UiMsg>(json).unwrap(), expected);
        }
        let m: UiMsg = serde_json::from_str(r#"{"type":"togglePanel","panel":"downloads"}"#).unwrap();
        assert_eq!(m, UiMsg::TogglePanel { panel: Panel::Downloads });
        let m: UiMsg = serde_json::from_str(r#"{"type":"togglePanel","panel":"bookmarks"}"#).unwrap();
        assert_eq!(m, UiMsg::TogglePanel { panel: Panel::Bookmarks });
        let m: UiMsg = serde_json::from_str(r#"{"type":"togglePanel","panel":"history"}"#).unwrap();
        assert_eq!(m, UiMsg::TogglePanel { panel: Panel::History });
        let m: UiMsg = serde_json::from_str(r#"{"type":"closePanel"}"#).unwrap();
        assert_eq!(m, UiMsg::ClosePanel);
        // Unknown values are rejected rather than guessed.
        assert!(serde_json::from_str::<UiMsg>(r#"{"type":"updateSettings","patch":{"searchEngine":"evil"}}"#).is_err());
        assert!(serde_json::from_str::<UiMsg>(r#"{"type":"updateSettings","patch":{"downloadDir":"C:\\"}}"#).is_err());
        assert!(serde_json::from_str::<UiMsg>(r#"{"type":"togglePanel","panel":"extensions"}"#).is_err());
    }

    #[test]
    fn parses_download_messages() {
        let m: UiMsg = serde_json::from_str(r#"{"type":"downloadAction","id":7,"action":"show"}"#).unwrap();
        assert_eq!(m, UiMsg::DownloadAction { id: 7, action: DownloadAction::Show });
        let m: UiMsg = serde_json::from_str(r#"{"type":"clearDownloads"}"#).unwrap();
        assert_eq!(m, UiMsg::ClearDownloads);
        assert!(serde_json::from_str::<UiMsg>(r#"{"type":"downloadAction","id":7,"action":"run"}"#).is_err());
    }

    #[test]
    fn parses_window_messages() {
        let m: UiMsg = serde_json::from_str(r#"{"type":"window","action":"toggleMaximize"}"#).unwrap();
        assert_eq!(m, UiMsg::Window { action: WindowAction::ToggleMaximize });
        let m: UiMsg = serde_json::from_str(r#"{"type":"window","action":"resizeTopLeft"}"#).unwrap();
        assert_eq!(m, UiMsg::Window { action: WindowAction::ResizeTopLeft });
        assert!(serde_json::from_str::<UiMsg>(r#"{"type":"window","action":"explode"}"#).is_err());
    }

    /// These are the exact messages `ui/index.html` sends. Field names with
    /// more than one word are easy to get wrong between JavaScript
    /// (`newTab`) and Rust (`new_tab`), and a mismatch is silent: the
    /// message just fails to parse and the click does nothing.
    #[test]
    fn parses_multi_word_fields_as_the_ui_sends_them() {
        let m: UiMsg =
            serde_json::from_str(r#"{"type":"openUrl","url":"https://example.com/","newTab":true}"#).unwrap();
        assert_eq!(m, UiMsg::OpenUrl { url: "https://example.com/".into(), new_tab: true });
        let m: UiMsg = serde_json::from_str(r#"{"type":"reorderTab","id":4,"toIndex":2}"#).unwrap();
        assert_eq!(m, UiMsg::ReorderTab { id: 4, to_index: 2 });
    }

    #[test]
    fn parses_bookmark_history_and_tab_messages() {
        for (json, expected) in [
            (r#"{"type":"stop"}"#, UiMsg::Stop),
            (r#"{"type":"reopenClosedTab"}"#, UiMsg::ReopenClosedTab),
            (r#"{"type":"resetZoom"}"#, UiMsg::ResetZoom),
            (r#"{"type":"toggleBookmark"}"#, UiMsg::ToggleBookmark),
            (r#"{"type":"clearHistory"}"#, UiMsg::ClearHistory),
            (r#"{"type":"confirmClose"}"#, UiMsg::ConfirmClose),
            (r#"{"type":"cancelClose"}"#, UiMsg::CancelClose),
            (
                r#"{"type":"removeBookmark","url":"https://a.example/"}"#,
                UiMsg::RemoveBookmark { url: "https://a.example/".into() },
            ),
            (
                r#"{"type":"removeHistory","url":"https://a.example/"}"#,
                UiMsg::RemoveHistory { url: "https://a.example/".into() },
            ),
            (r#"{"type":"querySuggestions","query":"rus"}"#, UiMsg::QuerySuggestions { query: "rus".into() }),
        ] {
            assert_eq!(serde_json::from_str::<UiMsg>(json).unwrap(), expected, "{json}");
        }
    }

    #[test]
    fn parses_menu_and_command_messages() {
        let m: UiMsg = serde_json::from_str(r#"{"type":"command","command":"newPrivateTab"}"#).unwrap();
        assert_eq!(m, UiMsg::Command { command: Command::NewPrivateTab });
        let m: UiMsg = serde_json::from_str(r#"{"type":"command","command":"savePage"}"#).unwrap();
        assert_eq!(m, UiMsg::Command { command: Command::SavePage });
        let m: UiMsg = serde_json::from_str(r#"{"type":"command","command":"toggleBookmarksBar"}"#).unwrap();
        assert_eq!(m, UiMsg::Command { command: Command::ToggleBookmarksBar });
        let m: UiMsg = serde_json::from_str(r#"{"type":"command","command":"toggleSplitView"}"#).unwrap();
        assert_eq!(m, UiMsg::Command { command: Command::ToggleSplitView });
        let m: UiMsg = serde_json::from_str(r#"{"type":"selectTab","id":5,"focus":false}"#).unwrap();
        assert_eq!(m, UiMsg::SelectTab { id: 5, focus: false });
        let m: UiMsg = serde_json::from_str(r#"{"type":"selectTab","id":5}"#).unwrap();
        assert_eq!(m, UiMsg::SelectTab { id: 5, focus: true });
        let m: UiMsg = serde_json::from_str(r#"{"type":"menuCommand","command":"zoomIn","close":false}"#).unwrap();
        assert_eq!(m, UiMsg::MenuCommand { command: Command::ZoomIn, close: false });
        let m: UiMsg = serde_json::from_str(r#"{"type":"tabMenuCommand","id":2,"command":"duplicate"}"#).unwrap();
        assert_eq!(m, UiMsg::TabMenuCommand { id: 2, command: TabCommand::Duplicate });
        let m: UiMsg = serde_json::from_str(r#"{"type":"tabMenuCommand","id":2,"command":"togglePin"}"#).unwrap();
        assert_eq!(m, UiMsg::TabMenuCommand { id: 2, command: TabCommand::TogglePin });
        assert_eq!(serde_json::from_str::<UiMsg>(r#"{"type":"closeMenu"}"#).unwrap(), UiMsg::CloseMenu);
        let m: UiMsg = serde_json::from_str(r#"{"type":"showMenu","x":1180.5,"y":38}"#).unwrap();
        assert_eq!(m, UiMsg::ShowMenu { x: 1180.5, y: 38.0 });
        let m: UiMsg = serde_json::from_str(r#"{"type":"tabMenu","id":3,"x":40,"y":20}"#).unwrap();
        assert_eq!(m, UiMsg::TabMenu { id: 3, x: 40.0, y: 20.0 });
        let m: UiMsg = serde_json::from_str(r#"{"type":"toggleMute","id":3}"#).unwrap();
        assert_eq!(m, UiMsg::ToggleMute { id: 3 });
        let m: UiMsg = serde_json::from_str(r#"{"type":"noticeAction","id":7}"#).unwrap();
        assert_eq!(m, UiMsg::NoticeAction { id: 7 });
        assert_eq!(serde_json::from_str::<UiMsg>(r#"{"type":"checkForUpdates"}"#).unwrap(), UiMsg::CheckForUpdates);
        assert_eq!(serde_json::from_str::<UiMsg>(r#"{"type":"installUpdate"}"#).unwrap(), UiMsg::InstallUpdate);
        assert_eq!(
            serde_json::from_str::<UiMsg>(r#"{"type":"importBookmarksFile"}"#).unwrap(),
            UiMsg::ImportBookmarksFile
        );
        assert_eq!(serde_json::from_str::<UiMsg>(r#"{"type":"exportBookmarks"}"#).unwrap(), UiMsg::ExportBookmarks);
        assert_eq!(
            serde_json::from_str::<UiMsg>(r#"{"type":"importFrom","source":"edge"}"#).unwrap(),
            UiMsg::ImportFrom { source: ImportSource::Edge }
        );
        assert!(serde_json::from_str::<UiMsg>(r#"{"type":"importFrom","source":"netscape"}"#).is_err());
        let m: UiMsg = serde_json::from_str(r#"{"type":"command","command":"viewSource"}"#).unwrap();
        assert_eq!(m, UiMsg::Command { command: Command::ViewSource });
        let m: UiMsg = serde_json::from_str(r#"{"type":"command","command":"clearBrowsingData"}"#).unwrap();
        assert_eq!(m, UiMsg::Command { command: Command::ClearBrowsingData });
        let m: UiMsg = serde_json::from_str(r#"{"type":"command","command":"readerMode"}"#).unwrap();
        assert_eq!(m, UiMsg::Command { command: Command::ReaderMode });
        assert!(serde_json::from_str::<UiMsg>(r#"{"type":"command","command":"formatDisk"}"#).is_err());
    }

    #[test]
    fn parses_memory_manager_messages() {
        for (json, expected) in [
            (r#"{"type":"showMemoryPanel","x":900,"y":80.5}"#, UiMsg::ShowMemoryPanel { x: 900.0, y: 80.5 }),
            (r#"{"type":"memoryPanelRefresh"}"#, UiMsg::MemoryPanelRefresh),
            (r#"{"type":"sleepTab","id":7}"#, UiMsg::SleepTab { id: 7 }),
            (r#"{"type":"sleepBackgroundTabs"}"#, UiMsg::SleepBackgroundTabs),
        ] {
            assert_eq!(serde_json::from_str::<UiMsg>(json).unwrap(), expected, "{json}");
        }
    }

    #[test]
    fn parses_tab_hover_messages() {
        let m: UiMsg = serde_json::from_str(r#"{"type":"tabHover","id":4,"x":120.5,"y":38}"#).unwrap();
        assert_eq!(m, UiMsg::TabHover { id: 4, x: 120.5, y: 38.0 });
        let m: UiMsg = serde_json::from_str(r#"{"type":"tabHoverEnd"}"#).unwrap();
        assert_eq!(m, UiMsg::TabHoverEnd);
    }

    #[test]
    fn rejects_malformed_messages() {
        for json in
            [r#"{"type":"formatDisk"}"#, r#"{"type":"closeTab"}"#, r#"{"type":"closeTab","id":"x"}"#, "not json", ""]
        {
            assert!(serde_json::from_str::<UiMsg>(json).is_err(), "{json}");
        }
    }
}
