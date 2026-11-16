//! User settings, saved as JSON in `%LOCALAPPDATA%\RustBrowser\settings.json`.

use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize};

use crate::url_input;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchEngine {
    #[default]
    Google,
    Bing,
    DuckDuckGo,
    Brave,
    Ecosia,
}

impl SearchEngine {
    /// Every engine, in the order shown in Settings.
    pub const ALL: [SearchEngine; 5] =
        [SearchEngine::Google, SearchEngine::Bing, SearchEngine::DuckDuckGo, SearchEngine::Brave, SearchEngine::Ecosia];

    pub fn name(self) -> &'static str {
        match self {
            SearchEngine::Google => "Google",
            SearchEngine::Bing => "Bing",
            SearchEngine::DuckDuckGo => "DuckDuckGo",
            SearchEngine::Brave => "Brave Search",
            SearchEngine::Ecosia => "Ecosia",
        }
    }

    /// The URL-encoded query is appended to this.
    pub fn search_prefix(self) -> &'static str {
        match self {
            SearchEngine::Google => "https://www.google.com/search?q=",
            SearchEngine::Bing => "https://www.bing.com/search?q=",
            SearchEngine::DuckDuckGo => "https://duckduckgo.com/?q=",
            SearchEngine::Brave => "https://search.brave.com/search?q=",
            SearchEngine::Ecosia => "https://www.ecosia.org/search?q=",
        }
    }

    /// The engine's front page; the home page unless the user set their own.
    pub fn home_url(self) -> &'static str {
        match self {
            SearchEngine::Google => "https://www.google.com/",
            SearchEngine::Bing => "https://www.bing.com/",
            SearchEngine::DuckDuckGo => "https://duckduckgo.com/",
            SearchEngine::Brave => "https://search.brave.com/",
            SearchEngine::Ecosia => "https://www.ecosia.org/",
        }
    }
}

/// Colours of the browser itself and what web pages are told to prefer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    /// Follow Windows.
    #[default]
    System,
    Light,
    Dark,
}

/// What opens when the browser starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Startup {
    #[default]
    HomePage,
    /// The tabs that were open last time.
    Continue,
    NewTab,
}

/// What a new tab shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NewTabPage {
    #[default]
    Blank,
    HomePage,
}

/// How hard the browser works to free memory used by tabs you aren't looking at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemorySaver {
    /// Background tabs only get a lower memory priority.
    Off,
    /// Freeze background tabs after 30 seconds, put them to sleep after 30 minutes.
    #[default]
    Balanced,
    /// Freeze background tabs after 10 seconds, put them to sleep after 5 minutes.
    Maximum,
}

impl MemorySaver {
    /// How long a tab must be out of sight before it is frozen and before it
    /// is put to sleep, or `None` when the memory saver is off.
    pub fn timings(self) -> Option<(Duration, Duration)> {
        match self {
            MemorySaver::Off => None,
            MemorySaver::Balanced => Some((Duration::from_secs(30), Duration::from_secs(30 * 60))),
            MemorySaver::Maximum => Some((Duration::from_secs(10), Duration::from_secs(5 * 60))),
        }
    }
}

/// Efficiency mode: the strongest memory saver, no animations and lighter
/// visual effects, for PCs with little memory or a slow processor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EfficiencyMode {
    /// On for low-end PCs (see `device::is_low_end`), off otherwise.
    #[default]
    Auto,
    On,
    Off,
}

/// How strongly WebView2 blocks trackers (cookies and scripts that follow
/// you across websites).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackingPrevention {
    Off,
    Basic,
    /// WebView2's own default.
    #[default]
    Balanced,
    Strict,
}

/// Page zoom levels offered in Settings, in percent.
pub const ZOOM_LEVELS: [u16; 11] = [50, 67, 75, 80, 90, 100, 110, 125, 150, 175, 200];

/// Steps used by Zoom in / Zoom out (the same ones Chrome uses).
pub const ZOOM_STEPS: [u16; 15] = [25, 33, 50, 67, 75, 80, 90, 100, 110, 125, 150, 175, 200, 250, 300];

/// The next zoom step above (`up`) or below the current zoom. A zoom that
/// isn't on a step (e.g. from Ctrl+mouse wheel) moves to the nearest step in
/// that direction. Stays put at either end.
pub fn zoom_step(current: u16, up: bool) -> u16 {
    if up {
        ZOOM_STEPS.iter().copied().find(|&s| s > current).unwrap_or(current)
    } else {
        ZOOM_STEPS.iter().rev().copied().find(|&s| s < current).unwrap_or(current)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub search_engine: SearchEngine,
    pub theme: Theme,
    pub startup: Startup,
    pub new_tab_page: NewTabPage,
    /// A full URL, or empty for "my search engine's front page".
    pub home_page: String,
    pub show_home_button: bool,
    pub show_bookmarks_bar: bool,
    /// One of [`ZOOM_LEVELS`].
    pub zoom_percent: u16,
    /// `None` uses the Windows Downloads folder.
    pub download_dir: Option<PathBuf>,
    /// Block windows that pages open without being clicked.
    pub block_popups: bool,
    pub memory_saver: MemorySaver,
    pub tracking_prevention: TrackingPrevention,
    /// Let WebView2 offer to remember passwords typed into sign-in forms.
    pub save_passwords: bool,
    /// Let WebView2 suggest addresses and other form entries.
    pub autofill: bool,
    /// Microsoft Defender SmartScreen: warns before phishing and malware
    /// sites and dangerous downloads.
    pub safe_browsing: bool,
    /// Once a day, ask GitHub whether a newer version is out (and show an
    /// Update button if so). Nothing is installed without a click.
    pub check_for_updates: bool,
    /// The built-in ad and tracker blocker (see `adblock.rs`).
    pub block_ads: bool,
    pub efficiency_mode: EfficiencyMode,
    /// Zoom chosen for particular websites (host without "www." → percent).
    /// Sites not listed use `zoom_percent`. Set by zooming a page.
    pub site_zoom: BTreeMap<String, u16>,
}

/// The lowest and highest page zoom kept for a website, in percent.
pub const SITE_ZOOM_RANGE: std::ops::RangeInclusive<u16> = 25..=500;

impl Default for Settings {
    fn default() -> Self {
        Self {
            search_engine: SearchEngine::default(),
            theme: Theme::default(),
            startup: Startup::default(),
            new_tab_page: NewTabPage::default(),
            home_page: String::new(),
            show_home_button: false,
            show_bookmarks_bar: false,
            zoom_percent: 100,
            download_dir: None,
            block_popups: true,
            memory_saver: MemorySaver::default(),
            tracking_prevention: TrackingPrevention::default(),
            save_passwords: true,
            autofill: true,
            safe_browsing: true,
            check_for_updates: true,
            block_ads: true,
            efficiency_mode: EfficiencyMode::Auto,
            site_zoom: BTreeMap::new(),
        }
    }
}

/// A change to some settings, sent by the Settings panel. Fields left out
/// stay as they are.
#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct SettingsPatch {
    pub search_engine: Option<SearchEngine>,
    pub theme: Option<Theme>,
    pub startup: Option<Startup>,
    pub new_tab_page: Option<NewTabPage>,
    /// Whatever the user typed; cleaned up by [`Settings::apply`].
    pub home_page: Option<String>,
    pub show_home_button: Option<bool>,
    pub show_bookmarks_bar: Option<bool>,
    pub zoom_percent: Option<u16>,
    pub block_popups: Option<bool>,
    pub memory_saver: Option<MemorySaver>,
    pub tracking_prevention: Option<TrackingPrevention>,
    pub save_passwords: Option<bool>,
    pub autofill: Option<bool>,
    pub safe_browsing: Option<bool>,
    pub check_for_updates: Option<bool>,
    pub block_ads: Option<bool>,
    pub efficiency_mode: Option<EfficiencyMode>,
}

impl Settings {
    /// Loads settings; a missing or unreadable file gives the defaults.
    pub fn load(path: &Path) -> Self {
        let mut settings: Settings =
            fs::read_to_string(path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default();
        // The file can be edited by hand: never trust a zoom we don't offer.
        if !ZOOM_LEVELS.contains(&settings.zoom_percent) {
            settings.zoom_percent = 100;
        }
        settings.site_zoom.retain(|host, zoom| !host.is_empty() && SITE_ZOOM_RANGE.contains(zoom));
        settings
    }

    /// The zoom for a page of `host` (as given by [`zoom_site`]).
    pub fn zoom_for(&self, host: Option<&str>) -> u16 {
        host.and_then(|h| self.site_zoom.get(h).copied()).unwrap_or(self.zoom_percent)
    }

    /// Remembers that `host` is shown at `percent`: forgotten again when that's
    /// the normal zoom. Returns `true` if anything changed.
    pub fn remember_site_zoom(&mut self, host: &str, percent: u16) -> bool {
        if percent == self.zoom_percent || !SITE_ZOOM_RANGE.contains(&percent) {
            self.site_zoom.remove(host).is_some()
        } else {
            self.site_zoom.insert(host.to_string(), percent) != Some(percent)
        }
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        write_atomic(path, &serde_json::to_string_pretty(self)?)
    }

    /// The page behind the Home button and "open the home page".
    pub fn home_url(&self) -> String {
        if self.home_page.is_empty() {
            self.search_engine.home_url().to_string()
        } else {
            self.home_page.clone()
        }
    }

    /// Applies a change. Invalid values are ignored. Returns `true` if
    /// anything actually changed.
    pub fn apply(&mut self, patch: SettingsPatch) -> bool {
        let before = self.clone();
        if let Some(engine) = patch.search_engine {
            self.search_engine = engine;
        }
        if let Some(theme) = patch.theme {
            self.theme = theme;
        }
        if let Some(startup) = patch.startup {
            self.startup = startup;
        }
        if let Some(page) = patch.new_tab_page {
            self.new_tab_page = page;
        }
        if let Some(text) = patch.home_page {
            // Same rules as the address bar, so "example.com" works.
            // An empty box goes back to the search engine's front page.
            self.home_page = url_input::resolve(&text, self.search_engine.search_prefix()).unwrap_or_default();
        }
        if let Some(show) = patch.show_home_button {
            self.show_home_button = show;
        }
        if let Some(show) = patch.show_bookmarks_bar {
            self.show_bookmarks_bar = show;
        }
        if let Some(zoom) = patch.zoom_percent {
            if ZOOM_LEVELS.contains(&zoom) {
                self.zoom_percent = zoom;
            }
        }
        if let Some(block) = patch.block_popups {
            self.block_popups = block;
        }
        if let Some(level) = patch.tracking_prevention {
            self.tracking_prevention = level;
        }
        if let Some(saver) = patch.memory_saver {
            self.memory_saver = saver;
        }
        if let Some(save) = patch.save_passwords {
            self.save_passwords = save;
        }
        if let Some(autofill) = patch.autofill {
            self.autofill = autofill;
        }
        if let Some(safe) = patch.safe_browsing {
            self.safe_browsing = safe;
        }
        if let Some(check) = patch.check_for_updates {
            self.check_for_updates = check;
        }
        if let Some(block) = patch.block_ads {
            self.block_ads = block;
        }
        if let Some(mode) = patch.efficiency_mode {
            self.efficiency_mode = mode;
        }
        *self != before
    }
}

/// The website whose zoom a page uses: its host without a leading "www.", or
/// `None` for pages that aren't websites (the New Tab page, files).
pub fn zoom_site(url: &str) -> Option<String> {
    let host = crate::adblock::host_of(url)?;
    Some(host.strip_prefix("www.").unwrap_or(&host).to_string())
}

/// Writes to a temporary file first, then renames it over the real one, so a
/// crash mid-save can't leave a half-written file behind. Retries briefly if
/// Windows antivirus or search indexing holds a transient lock on the target.
pub(crate) fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    fs::write(&tmp, text)?;
    for attempt in 0..5 {
        match fs::rename(&tmp, path) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied && attempt < 4 => {
                std::thread::sleep(std::time::Duration::from_millis(15));
            }
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                return Err(e);
            }
        }
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("rust-browser-{name}-{}.json", std::process::id()))
    }

    #[test]
    fn defaults() {
        let s = Settings::default();
        assert_eq!(s.search_engine, SearchEngine::Google);
        assert_eq!(s.theme, Theme::System);
        assert_eq!(s.startup, Startup::HomePage);
        assert_eq!(s.new_tab_page, NewTabPage::Blank);
        assert_eq!(s.zoom_percent, 100);
        assert_eq!(s.home_url(), "https://www.google.com/");
        assert!(!s.show_home_button);
        assert!(!s.show_bookmarks_bar);
        assert_eq!(s.download_dir, None);
        // Privacy defaults: pop-ups blocked, WebView2's standard tracking
        // prevention, and the usual browser conveniences switched on.
        assert!(s.block_popups);
        assert_eq!(s.tracking_prevention, TrackingPrevention::Balanced);
        assert!(s.save_passwords);
        assert!(s.autofill);
    }

    #[test]
    fn older_files_get_privacy_defaults() {
        let s: Settings = serde_json::from_str(r#"{"searchEngine":"google","theme":"dark"}"#).unwrap();
        assert!(s.block_popups && s.save_passwords && s.autofill);
        assert_eq!(s.tracking_prevention, TrackingPrevention::Balanced);
        // SmartScreen protection and update checks are on unless the user turns them off.
        assert!(s.safe_browsing);
        assert!(s.check_for_updates);
        let mut off = s.clone();
        let patch: SettingsPatch = serde_json::from_str(r#"{"checkForUpdates":false}"#).unwrap();
        assert!(off.apply(patch));
        assert!(!off.check_for_updates);
        // The ad blocker is on by default and can be switched off.
        assert!(s.block_ads);
        let patch: SettingsPatch = serde_json::from_str(r#"{"blockAds":false}"#).unwrap();
        assert!(off.apply(patch));
        assert!(!off.block_ads);
        // Efficiency mode follows the PC unless chosen.
        assert_eq!(s.efficiency_mode, EfficiencyMode::Auto);
        let patch: SettingsPatch = serde_json::from_str(r#"{"efficiencyMode":"on"}"#).unwrap();
        assert!(off.apply(patch));
        assert_eq!(off.efficiency_mode, EfficiencyMode::On);
        assert!(serde_json::from_str::<SettingsPatch>(r#"{"efficiencyMode":"turbo"}"#).is_err());
        let mut s = s;
        assert!(s.apply(SettingsPatch { safe_browsing: Some(false), ..Default::default() }));
        assert!(!s.safe_browsing);
    }

    #[test]
    fn privacy_settings_can_be_changed() {
        let mut s = Settings::default();
        let patch: SettingsPatch = serde_json::from_str(
            r#"{"blockPopups":false,"trackingPrevention":"strict","savePasswords":false,"autofill":false}"#,
        )
        .unwrap();
        assert!(s.apply(patch));
        assert!(!s.block_popups && !s.save_passwords && !s.autofill);
        assert_eq!(s.tracking_prevention, TrackingPrevention::Strict);
        assert!(serde_json::from_str::<SettingsPatch>(r#"{"trackingPrevention":"extreme"}"#).is_err());
    }

    #[test]
    fn memory_saver_defaults_to_balanced_and_has_sane_timings() {
        assert_eq!(Settings::default().memory_saver, MemorySaver::Balanced);
        let s: Settings = serde_json::from_str(r#"{"searchEngine":"google"}"#).unwrap();
        assert_eq!(s.memory_saver, MemorySaver::Balanced, "older settings files get the default");
        assert_eq!(MemorySaver::Off.timings(), None);
        for saver in [MemorySaver::Balanced, MemorySaver::Maximum] {
            let (freeze, sleep) = saver.timings().unwrap();
            assert!(freeze < sleep, "{saver:?}: a tab is frozen before it is put to sleep");
        }
        let (_, balanced_sleep) = MemorySaver::Balanced.timings().unwrap();
        let (_, maximum_sleep) = MemorySaver::Maximum.timings().unwrap();
        assert!(maximum_sleep < balanced_sleep);
        let patch: SettingsPatch = serde_json::from_str(r#"{"memorySaver":"maximum"}"#).unwrap();
        let mut s = Settings::default();
        assert!(s.apply(patch));
        assert_eq!(s.memory_saver, MemorySaver::Maximum);
        assert!(serde_json::from_str::<SettingsPatch>(r#"{"memorySaver":"turbo"}"#).is_err());
    }

    #[test]
    fn remembers_zoom_per_site() {
        let mut s = Settings::default();
        assert_eq!(zoom_site("https://www.Example.com/page").as_deref(), Some("example.com"));
        assert_eq!(zoom_site("https://news.example.com/").as_deref(), Some("news.example.com"));
        assert_eq!(zoom_site("about:blank"), None);
        assert_eq!(s.zoom_for(Some("example.com")), 100);
        assert!(s.remember_site_zoom("example.com", 150));
        assert!(!s.remember_site_zoom("example.com", 150), "same zoom again is no change");
        assert_eq!(s.zoom_for(Some("example.com")), 150);
        assert_eq!(s.zoom_for(Some("other.org")), 100);
        assert_eq!(s.zoom_for(None), 100);
        // Going back to the normal zoom forgets the site.
        assert!(s.remember_site_zoom("example.com", 100));
        assert!(s.site_zoom.is_empty());
        assert!(!s.remember_site_zoom("example.com", 9000), "out of range is ignored");
        // Damaged entries in the file are dropped.
        let path = temp_file("settings-site-zoom");
        fs::write(&path, r#"{"siteZoom":{"ok.com":125,"bad.com":5000,"":110}}"#).unwrap();
        assert_eq!(Settings::load(&path).site_zoom, BTreeMap::from([("ok.com".to_string(), 125)]));
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn zoom_steps_up_and_down() {
        assert_eq!(zoom_step(100, true), 110);
        assert_eq!(zoom_step(100, false), 90);
        // Off-step zoom (e.g. Ctrl+mouse wheel) moves to the nearest step.
        assert_eq!(zoom_step(104, true), 110);
        assert_eq!(zoom_step(104, false), 100);
        // Ends of the range stay put.
        assert_eq!(zoom_step(300, true), 300);
        assert_eq!(zoom_step(25, false), 25);
    }

    #[test]
    fn values_use_stable_ids() {
        assert_eq!(serde_json::to_string(&SearchEngine::DuckDuckGo).unwrap(), r#""duckduckgo""#);
        assert_eq!(serde_json::to_string(&Theme::Dark).unwrap(), r#""dark""#);
        assert_eq!(serde_json::to_string(&Startup::Continue).unwrap(), r#""continue""#);
        assert_eq!(serde_json::to_string(&Startup::HomePage).unwrap(), r#""homePage""#);
        assert_eq!(serde_json::to_string(&NewTabPage::HomePage).unwrap(), r#""homePage""#);
        let s: Settings = serde_json::from_str(r#"{"searchEngine":"bing","theme":"light"}"#).unwrap();
        assert_eq!((s.search_engine, s.theme), (SearchEngine::Bing, Theme::Light));
    }

    #[test]
    fn older_settings_files_still_load() {
        // A file written before themes etc. existed keeps its values and
        // gets defaults for everything new.
        let s: Settings = serde_json::from_str(r#"{"searchEngine":"brave"}"#).unwrap();
        assert_eq!(s, Settings { search_engine: SearchEngine::Brave, ..Settings::default() });
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn save_then_load_round_trips() {
        let path = temp_file("settings-roundtrip");
        let saved = Settings {
            search_engine: SearchEngine::Brave,
            theme: Theme::Dark,
            startup: Startup::Continue,
            new_tab_page: NewTabPage::HomePage,
            home_page: "https://example.com/".into(),
            show_home_button: true,
            show_bookmarks_bar: true,
            zoom_percent: 125,
            download_dir: Some(PathBuf::from(r"D:\Files")),
            block_popups: false,
            memory_saver: MemorySaver::Maximum,
            tracking_prevention: TrackingPrevention::Strict,
            save_passwords: false,
            autofill: false,
            safe_browsing: false,
            check_for_updates: false,
            block_ads: false,
            efficiency_mode: EfficiencyMode::On,
            site_zoom: BTreeMap::from([("example.com".to_string(), 150)]),
        };
        saved.save(&path).unwrap();
        assert_eq!(Settings::load(&path), saved);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn broken_or_missing_file_gives_defaults() {
        let path = temp_file("settings-bad");
        fs::write(&path, "{ not json").unwrap();
        assert_eq!(Settings::load(&path), Settings::default());
        fs::write(&path, r#"{"searchEngine":"altavista"}"#).unwrap();
        assert_eq!(Settings::load(&path), Settings::default());
        fs::write(&path, r#"{"theme":"dark","zoomPercent":9999}"#).unwrap();
        assert_eq!(Settings::load(&path), Settings { theme: Theme::Dark, ..Settings::default() });
        let _ = fs::remove_file(&path);
        assert_eq!(Settings::load(&path), Settings::default());
    }

    #[test]
    fn patch_changes_only_what_it_names() {
        let mut s = Settings::default();
        let patch: SettingsPatch = serde_json::from_str(r#"{"theme":"dark","zoomPercent":150}"#).unwrap();
        assert!(s.apply(patch));
        assert_eq!(s, Settings { theme: Theme::Dark, zoom_percent: 150, ..Settings::default() });

        // Applying the same thing again is not a change.
        let patch: SettingsPatch = serde_json::from_str(r#"{"theme":"dark"}"#).unwrap();
        assert!(!s.apply(patch));
    }

    #[test]
    fn patch_rejects_bad_values() {
        let mut s = Settings::default();
        assert!(!s.apply(SettingsPatch { zoom_percent: Some(101), ..Default::default() }));
        assert_eq!(s.zoom_percent, 100);
        assert!(serde_json::from_str::<SettingsPatch>(r#"{"theme":"neon"}"#).is_err());
        assert!(serde_json::from_str::<SettingsPatch>(r#"{"downloadDir":"C:\\Windows"}"#).is_err());
        assert!(serde_json::from_str::<SettingsPatch>(r#"{"unknown":1}"#).is_err());
    }

    #[test]
    fn home_page_is_cleaned_up_like_the_address_bar() {
        let mut s = Settings::default();
        assert!(s.apply(SettingsPatch { home_page: Some("  example.com ".into()), ..Default::default() }));
        assert_eq!(s.home_page, "https://example.com");
        assert_eq!(s.home_url(), "https://example.com");

        // Clearing the box goes back to the search engine's front page.
        assert!(s.apply(SettingsPatch { home_page: Some("   ".into()), ..Default::default() }));
        assert_eq!(s.home_page, "");
        s.search_engine = SearchEngine::Bing;
        assert_eq!(s.home_url(), "https://www.bing.com/");
    }

    #[test]
    fn every_engine_has_https_urls() {
        for engine in SearchEngine::ALL {
            assert!(engine.search_prefix().starts_with("https://"), "{engine:?}");
            assert!(engine.search_prefix().ends_with("q="), "{engine:?}");
            assert!(engine.home_url().starts_with("https://"), "{engine:?}");
            assert!(!engine.name().is_empty());
        }
    }

    #[test]
    fn zoom_levels_include_100_and_convert() {
        assert!(ZOOM_LEVELS.contains(&100));
        let s = Settings { zoom_percent: 125, ..Settings::default() };
        assert_eq!(s.zoom_for(None), 125, "pages without a site of their own use the normal zoom");
    }
}
