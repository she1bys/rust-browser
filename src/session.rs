//! The tabs that were open, saved in `%LOCALAPPDATA%\RustBrowser\session.json`
//! so "Continue where you left off" can bring them back.

use std::{fs, io, path::Path};

use serde::{Deserialize, Serialize};

use crate::settings::write_atomic;

/// More tabs than this are not restored; opening hundreds at once would
/// make startup crawl.
const MAX_TABS: usize = 50;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub tabs: Vec<String>,
    /// Optional saved titles matching `tabs`, shown while background tabs wait to load.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub titles: Vec<String>,
    /// Index into `tabs` of the tab that was showing.
    pub active: usize,
}

impl Session {
    /// Loads the saved session, keeping only addresses that are safe to
    /// reopen. A missing or broken file gives an empty session.
    pub fn load(path: &Path) -> Self {
        let saved: Session =
            fs::read_to_string(path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default();
        saved.sanitized()
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        write_atomic(path, &serde_json::to_string(self)?)
    }

    fn sanitized(self) -> Self {
        let mut tabs = Vec::new();
        let mut titles = Vec::new();
        let mut active = 0;
        let has_titles = !self.titles.is_empty();
        for (index, url) in self.tabs.into_iter().enumerate() {
            if tabs.len() == MAX_TABS {
                break;
            }
            if is_restorable(&url) {
                if index == self.active {
                    active = tabs.len();
                }
                tabs.push(url);
                if has_titles {
                    titles.push(self.titles.get(index).cloned().unwrap_or_default());
                }
            }
        }
        Session { tabs, titles, active }
    }
}

/// The file could have been edited by something else, so only reopen plain
/// web, file, and view-source addresses, never things like `javascript:`.
/// Also used for addresses opened from the bookmarks and history lists.
pub(crate) fn is_restorable(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    if let Some(inner) = lower.strip_prefix("view-source:") {
        return ["http://", "https://", "file:///"].iter().any(|scheme| inner.starts_with(scheme));
    }
    ["http://", "https://", "file:///"].iter().any(|scheme| lower.starts_with(scheme))
        || lower == "about:blank"
        || lower == "about:downloads"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(tabs: &[&str], active: usize) -> Session {
        Session { tabs: tabs.iter().map(|s| s.to_string()).collect(), titles: Vec::new(), active }
    }

    #[test]
    fn round_trips_through_a_file() {
        let path = std::env::temp_dir().join(format!("rust-browser-session-{}.json", std::process::id()));
        let saved = session(&["https://a.example/", "about:blank", "https://b.example/x?y=1"], 2);
        saved.save(&path).unwrap();
        assert_eq!(Session::load(&path), saved);

        fs::write(&path, "nonsense").unwrap();
        assert_eq!(Session::load(&path), Session::default());
        let _ = fs::remove_file(&path);
        assert_eq!(Session::load(&path), Session::default());
    }

    #[test]
    fn drops_unsafe_addresses_and_keeps_the_active_tab() {
        let cleaned =
            session(&["javascript:alert(1)", "https://a.example/", "data:text/html,x", "https://b.example/"], 3)
                .sanitized();
        assert_eq!(cleaned, session(&["https://a.example/", "https://b.example/"], 1));

        // If the active tab itself was dropped, fall back to the first one.
        let cleaned = session(&["https://a.example/", "javascript:alert(1)"], 1).sanitized();
        assert_eq!(cleaned, session(&["https://a.example/"], 0));

        // An out-of-range index can't cause trouble either.
        let cleaned = session(&["https://a.example/"], 99).sanitized();
        assert_eq!(cleaned.active, 0);
    }

    #[test]
    fn caps_the_number_of_tabs() {
        let many: Vec<String> = (0..MAX_TABS + 25).map(|i| format!("https://example.com/{i}")).collect();
        let cleaned = Session { tabs: many, titles: Vec::new(), active: MAX_TABS + 10 }.sanitized();
        assert_eq!(cleaned.tabs.len(), MAX_TABS);
        assert_eq!(cleaned.active, 0);
    }

    #[test]
    fn allows_view_source_of_safe_urls_and_keeps_titles() {
        let saved = Session {
            tabs: vec![
                "view-source:https://example.com".into(),
                "view-source:javascript:alert(1)".into(),
                "https://rust-lang.org/".into(),
            ],
            titles: vec!["Source".into(), "Bad".into(), "Rust".into()],
            active: 2,
        };
        let cleaned = saved.sanitized();
        assert_eq!(cleaned.tabs, vec!["view-source:https://example.com", "https://rust-lang.org/"]);
        assert_eq!(cleaned.titles, vec!["Source", "Rust"]);
        assert_eq!(cleaned.active, 1);
    }
}
