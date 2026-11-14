//! Browsing history, saved as JSON in `%LOCALAPPDATA%\RustBrowser\history.json`.

use std::{
    fs, io,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::settings::write_atomic;

const MAX_ENTRIES: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub url: String,
    pub title: String,
    pub visits: u32,
    pub last_visited: u64,
}

#[derive(Debug, Default)]
pub struct History {
    items: Vec<HistoryEntry>,
}

impl History {
    pub fn load(path: &Path) -> Self {
        let items: Vec<HistoryEntry> =
            fs::read_to_string(path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default();
        Self { items }
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        write_atomic(path, &serde_json::to_string(&self.items)?)
    }

    pub fn items(&self) -> &[HistoryEntry] {
        &self.items
    }

    /// Records a visit to `url`, moving it to the front. Ignores non-web/blank URLs.
    pub fn record(&mut self, url: &str, title: &str) -> bool {
        if !is_recordable(url) {
            return false;
        }
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        if let Some(pos) = self.items.iter().position(|e| e.url == url) {
            let mut entry = self.items.remove(pos);
            entry.visits = entry.visits.saturating_add(1);
            entry.last_visited = now;
            if !title.trim().is_empty() {
                entry.title = title.to_string();
            }
            self.items.insert(0, entry);
        } else {
            self.items.insert(
                0,
                HistoryEntry {
                    url: url.to_string(),
                    title: if title.trim().is_empty() { url.to_string() } else { title.to_string() },
                    visits: 1,
                    last_visited: now,
                },
            );
            self.items.truncate(MAX_ENTRIES);
        }
        true
    }

    /// Merges history brought in from another browser: newest first, keeping
    /// the most recent visit and highest visit count for pages in both, and
    /// at most the usual number of entries. Returns how many new pages it has.
    pub fn import(&mut self, entries: Vec<HistoryEntry>) -> usize {
        let mut new_urls = Vec::new();
        for entry in entries {
            if !is_recordable(&entry.url) {
                continue;
            }
            match self.items.iter_mut().find(|e| e.url == entry.url) {
                Some(existing) => {
                    existing.visits = existing.visits.max(entry.visits);
                    existing.last_visited = existing.last_visited.max(entry.last_visited);
                }
                None => {
                    new_urls.push(entry.url.clone());
                    self.items.push(entry);
                }
            }
        }
        self.items.sort_by_key(|e| std::cmp::Reverse(e.last_visited));
        self.items.truncate(MAX_ENTRIES);
        new_urls.iter().filter(|url| self.items.iter().any(|e| &e.url == *url)).count()
    }

    /// Updates the title of the most recent entry matching `url`.
    pub fn update_title(&mut self, url: &str, title: &str) -> bool {
        if title.trim().is_empty() {
            return false;
        }
        if let Some(entry) = self.items.iter_mut().find(|e| e.url == url) {
            if entry.title != title {
                entry.title = title.to_string();
                return true;
            }
        }
        false
    }

    pub fn remove(&mut self, url: &str) -> bool {
        let before = self.items.len();
        self.items.retain(|e| e.url != url);
        self.items.len() != before
    }

    pub fn clear(&mut self) -> bool {
        if self.items.is_empty() {
            return false;
        }
        self.items.clear();
        true
    }

    pub fn search(&self, query: &str, limit: usize) -> Vec<&HistoryEntry> {
        let q = query.trim().to_ascii_lowercase();
        if q.is_empty() {
            return self.items.iter().take(limit).collect();
        }
        self.items
            .iter()
            .filter(|e| e.url.to_ascii_lowercase().contains(&q) || e.title.to_ascii_lowercase().contains(&q))
            .take(limit)
            .collect()
    }
}

fn is_recordable(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("file:///")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_deduplicates_and_caps() {
        let mut h = History::default();
        assert!(!h.record("about:blank", "Blank"));
        assert!(h.record("https://a.example/", "A"));
        assert!(h.record("https://b.example/", "B"));
        assert!(h.record("https://a.example/", "A2"));

        assert_eq!(h.items().len(), 2);
        assert_eq!(h.items()[0].url, "https://a.example/");
        assert_eq!(h.items()[0].visits, 2);
        assert_eq!(h.items()[0].title, "A2");

        for i in 0..MAX_ENTRIES + 20 {
            h.record(&format!("https://example.com/{i}"), "X");
        }
        assert_eq!(h.items().len(), MAX_ENTRIES);
    }

    #[test]
    fn imports_and_merges_by_date() {
        let mut h = History::default();
        h.record("https://a.example/", "A");
        let now = h.items()[0].last_visited;
        let entry =
            |url: &str, visits, last_visited| HistoryEntry { url: url.into(), title: url.into(), visits, last_visited };
        let added = h.import(vec![
            entry("https://a.example/", 9, 100),
            entry("https://b.example/", 2, now + 10),
            entry("chrome://settings/", 1, now),
        ]);
        assert_eq!(added, 1);
        assert_eq!(h.items()[0].url, "https://b.example/", "newest first");
        assert_eq!(h.items()[1].visits, 9, "highest visit count kept");
        assert_eq!(h.items()[1].last_visited, now, "most recent visit kept");
        assert_eq!(h.items().len(), 2);
    }

    #[test]
    fn searches_and_clears() {
        let mut h = History::default();
        h.record("https://rust-lang.org/", "Rust Programming Language");
        h.record("https://docs.rs/", "Docs.rs");
        assert_eq!(h.search("programming", 5).len(), 1);
        assert!(h.remove("https://docs.rs/"));
        assert_eq!(h.items().len(), 1);
        assert!(h.clear());
        assert!(!h.clear());
    }
}
