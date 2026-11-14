//! User bookmarks, saved as JSON in `%LOCALAPPDATA%\RustBrowser\bookmarks.json`.

use std::{fs, io, path::Path};

use serde::{Deserialize, Serialize};

use crate::settings::write_atomic;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bookmark {
    pub url: String,
    pub title: String,
}

#[derive(Debug, Default)]
pub struct Bookmarks {
    items: Vec<Bookmark>,
}

impl Bookmarks {
    pub fn load(path: &Path) -> Self {
        let items: Vec<Bookmark> =
            fs::read_to_string(path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default();
        Self { items }
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        write_atomic(path, &serde_json::to_string_pretty(&self.items)?)
    }

    pub fn items(&self) -> &[Bookmark] {
        &self.items
    }

    pub fn contains(&self, url: &str) -> bool {
        !url.is_empty() && self.items.iter().any(|b| b.url == url)
    }

    /// Adds or removes `url`. Returns `true` if the URL is now bookmarked.
    pub fn toggle(&mut self, url: String, title: String) -> bool {
        if let Some(pos) = self.items.iter().position(|b| b.url == url) {
            self.items.remove(pos);
            false
        } else {
            let label = if title.trim().is_empty() { url.clone() } else { title };
            self.items.insert(0, Bookmark { url, title: label });
            true
        }
    }

    /// Adds bookmarks brought in from another browser, after the existing
    /// ones, skipping any already here. Returns how many were added.
    pub fn import(&mut self, bookmarks: Vec<Bookmark>) -> usize {
        let before = self.items.len();
        for bookmark in bookmarks {
            if !bookmark.url.is_empty() && !self.contains(&bookmark.url) {
                self.items.push(bookmark);
            }
        }
        self.items.len() - before
    }

    pub fn remove(&mut self, url: &str) -> bool {
        let before = self.items.len();
        self.items.retain(|b| b.url != url);
        self.items.len() != before
    }

    pub fn search(&self, query: &str, limit: usize) -> Vec<&Bookmark> {
        let q = query.trim().to_ascii_lowercase();
        if q.is_empty() {
            return self.items.iter().take(limit).collect();
        }
        self.items
            .iter()
            .filter(|b| b.url.to_ascii_lowercase().contains(&q) || b.title.to_ascii_lowercase().contains(&q))
            .take(limit)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_and_search_bookmarks() {
        let mut bm = Bookmarks::default();
        assert!(bm.toggle("https://rust-lang.org/".into(), "Rust".into()));
        assert!(bm.contains("https://rust-lang.org/"));
        assert_eq!(bm.search("rust", 5).len(), 1);

        assert!(!bm.toggle("https://rust-lang.org/".into(), "Rust".into()));
        assert!(!bm.contains("https://rust-lang.org/"));
    }

    #[test]
    fn round_trips_through_file() {
        let path = std::env::temp_dir().join(format!("rust-browser-bookmarks-{}.json", std::process::id()));
        let mut bm = Bookmarks::default();
        bm.toggle("https://example.com/".into(), "".into());
        assert_eq!(bm.items()[0].title, "https://example.com/");
        bm.save(&path).unwrap();

        let loaded = Bookmarks::load(&path);
        assert_eq!(loaded.items(), bm.items());
        let _ = fs::remove_file(&path);
    }
}
