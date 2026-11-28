//! The tabs that were open, saved in `%LOCALAPPDATA%\RustBrowser\session.json`
//! so "Continue where you left off" can bring them back.

use std::{fs, io, path::Path};

use serde::{Deserialize, Serialize};

use crate::settings::write_atomic;

/// More tabs than this are not restored; opening hundreds at once would
/// make startup crawl.
const MAX_TABS: usize = 50;
/// How many closes "Reopen closed tab" remembers (a close of several tabs at
/// once counts as one).
pub const MAX_CLOSED: usize = 25;
/// A saved tab icon longer than this (a big inline image) is left out; the
/// page brings its icon back when it loads.
const MAX_FAVICON_LEN: usize = 4096;

/// A closed tab, for "Reopen closed tab" (Ctrl+Shift+T).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClosedTab {
    pub url: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub favicon: String,
    /// Where it was in the tab strip when it closed.
    pub index: usize,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
    /// Private tabs are remembered while the browser runs, never on disk.
    #[serde(skip)]
    pub private: bool,
}

/// Tabs closed together (one tab, or all of "Close other tabs"): reopened together.
pub type ClosedGroup = Vec<ClosedTab>;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub tabs: Vec<String>,
    /// Optional saved titles matching `tabs`, shown while background tabs wait to load.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub titles: Vec<String>,
    /// Index into `tabs` of the tab that was showing.
    pub active: usize,
    /// How many of the first `tabs` are pinned (pinned tabs always come first).
    #[serde(skip_serializing_if = "is_zero")]
    pub pinned: usize,
    /// Recently closed tabs, oldest first (the last one reopens first).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub closed: Vec<ClosedGroup>,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
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
        let mut pinned = 0;
        let has_titles = !self.titles.is_empty();
        for (index, url) in self.tabs.into_iter().enumerate() {
            if tabs.len() == MAX_TABS {
                break;
            }
            if is_restorable(&url) {
                if index == self.active {
                    active = tabs.len();
                }
                // Dropped tabs don't count, so pinned tabs stay exactly the leading ones.
                if index < self.pinned {
                    pinned += 1;
                }
                tabs.push(url);
                if has_titles {
                    titles.push(self.titles.get(index).cloned().unwrap_or_default());
                }
            }
        }
        Session { tabs, titles, active, pinned, closed: sanitize_closed(self.closed) }
    }
}

/// Keeps the recently closed tabs that are safe to reopen: web, file and
/// view-source addresses (like the open tabs), sensible icons, and only the
/// most recent `MAX_CLOSED` closes. Private tabs are dropped.
pub fn sanitize_closed(groups: Vec<ClosedGroup>) -> Vec<ClosedGroup> {
    let mut kept: Vec<ClosedGroup> = groups
        .into_iter()
        .map(|group| {
            group
                .into_iter()
                .filter(|t| !t.private && is_restorable(&t.url) && t.url != "about:blank")
                .take(MAX_TABS)
                .map(|mut t| {
                    let icon = t.favicon.to_ascii_lowercase();
                    let safe_icon =
                        icon.starts_with("https://") || icon.starts_with("http://") || icon.starts_with("data:image/");
                    if !safe_icon || t.favicon.len() > MAX_FAVICON_LEN {
                        t.favicon.clear();
                    }
                    t
                })
                .collect::<ClosedGroup>()
        })
        .filter(|group| !group.is_empty())
        .collect();
    let extra = kept.len().saturating_sub(MAX_CLOSED);
    kept.drain(..extra);
    kept
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
        Session {
            tabs: tabs.iter().map(|s| s.to_string()).collect(),
            titles: Vec::new(),
            active,
            pinned: 0,
            closed: Vec::new(),
        }
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
        let cleaned = Session { tabs: many, titles: Vec::new(), active: MAX_TABS + 10, pinned: 0, closed: Vec::new() }
            .sanitized();
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
            pinned: 0,
            closed: Vec::new(),
        };
        let cleaned = saved.sanitized();
        assert_eq!(cleaned.tabs, vec!["view-source:https://example.com", "https://rust-lang.org/"]);
        assert_eq!(cleaned.titles, vec!["Source", "Rust"]);
        assert_eq!(cleaned.active, 1);
    }

    #[test]
    fn keeps_pinned_tabs_leading_after_dropping_bad_ones() {
        let saved = Session {
            tabs: vec![
                "https://mail.example/".into(),
                "javascript:alert(1)".into(),
                "https://chat.example/".into(),
                "https://news.example/".into(),
            ],
            titles: Vec::new(),
            active: 3,
            pinned: 3,
            closed: Vec::new(),
        };
        let cleaned = saved.sanitized();
        assert_eq!(cleaned.tabs.len(), 3);
        assert_eq!(cleaned.pinned, 2);
        assert_eq!(cleaned.active, 2);

        // A count bigger than the list (an edited file) is cut down to it.
        let cleaned = Session { pinned: 99, ..session(&["https://a.example/"], 0) }.sanitized();
        assert_eq!(cleaned.pinned, 1);

        // Files from before pinned tabs existed have none.
        let old: Session = serde_json::from_str(r#"{"tabs":["https://a.example/"],"active":0}"#).unwrap();
        assert_eq!(old.pinned, 0);
        // And sessions without pinned tabs don't write the field.
        assert!(!serde_json::to_string(&session(&["https://a.example/"], 0)).unwrap().contains("pinned"));
    }

    fn closed(url: &str, index: usize) -> ClosedTab {
        ClosedTab { url: url.into(), title: "T".into(), index, ..Default::default() }
    }

    #[test]
    fn remembers_closed_tabs_but_never_private_or_unsafe_ones() {
        let private = ClosedTab { private: true, ..closed("https://secret.example/", 0) };
        let groups = vec![
            vec![closed("https://a.example/", 2)],
            vec![private.clone()],
            vec![closed("javascript:alert(1)", 0), closed("https://b.example/", 1), private],
            vec![ClosedTab { favicon: "javascript:x".into(), pinned: true, ..closed("https://c.example/", 0) }],
            vec![ClosedTab {
                favicon: format!("data:image/png;base64,{}", "A".repeat(5000)),
                ..closed("https://d.example/", 4)
            }],
            vec![ClosedTab { favicon: "https://e.example/icon.png".into(), ..closed("https://e.example/", 3) }],
        ];
        let kept = sanitize_closed(groups);
        let urls: Vec<Vec<&str>> = kept.iter().map(|g| g.iter().map(|t| t.url.as_str()).collect()).collect();
        assert_eq!(
            urls,
            vec![
                vec!["https://a.example/"],
                vec!["https://b.example/"],
                vec!["https://c.example/"],
                vec!["https://d.example/"],
                vec!["https://e.example/"]
            ]
        );
        assert!(kept[2][0].pinned && kept[2][0].favicon.is_empty(), "unsafe icon dropped");
        assert!(kept[3][0].favicon.is_empty(), "huge icon dropped");
        assert_eq!(kept[4][0].favicon, "https://e.example/icon.png");

        // Only the most recent closes are kept.
        let many: Vec<ClosedGroup> =
            (0..MAX_CLOSED + 5).map(|i| vec![closed(&format!("https://x.example/{i}"), 0)]).collect();
        let kept = sanitize_closed(many);
        assert_eq!(kept.len(), MAX_CLOSED);
        assert_eq!(kept.last().unwrap()[0].url, format!("https://x.example/{}", MAX_CLOSED + 4));
    }

    #[test]
    fn closed_tabs_round_trip_without_private_ones() {
        let path = std::env::temp_dir().join(format!("rust-browser-session-closed-{}.json", std::process::id()));
        let mut saved = session(&["https://a.example/"], 0);
        saved.closed = vec![vec![closed("https://b.example/", 1), closed("https://c.example/", 2)]];
        saved.save(&path).unwrap();
        assert_eq!(Session::load(&path), saved);
        // The private flag is never written, so a private tab can't come back from disk.
        assert!(!fs::read_to_string(&path).unwrap().contains("private"));
        let _ = fs::remove_file(&path);
        // Older files without closed tabs load as before.
        let old: Session = serde_json::from_str(r#"{"tabs":["https://a.example/"],"active":0}"#).unwrap();
        assert!(old.closed.is_empty());
    }
}
