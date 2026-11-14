//! The list of downloads shown in the Downloads panel, saved as JSON in
//! `%LOCALAPPDATA%\RustBrowser\downloads.json`.
//!
//! This is plain bookkeeping; the part that talks to WebView2 lives in
//! `download_hook.rs`.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::ipc::DownloadId;
use crate::settings::write_atomic;

/// Finished downloads beyond this many are dropped from the list, oldest first.
/// (The files themselves are never touched.)
const MAX_ITEMS: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DownloadState {
    InProgress,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Download {
    pub id: DownloadId,
    pub url: String,
    pub path: PathBuf,
    pub received: i64,
    /// Total size in bytes, or a value <= 0 when the server didn't say.
    pub total: i64,
    pub state: DownloadState,
    /// Started from a private tab: listed while the browser runs, never saved.
    #[serde(skip)]
    pub private: bool,
}

impl Download {
    pub fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.to_string_lossy().into_owned())
    }
}

/// Newest download first.
#[derive(Debug, Default)]
pub struct Downloads {
    items: Vec<Download>,
}

impl Downloads {
    /// Loads the saved list. Anything still marked in progress was cut off
    /// when the browser last closed, so it is shown as failed.
    pub fn load(path: &Path) -> Self {
        let mut items: Vec<Download> =
            fs::read_to_string(path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default();
        for item in &mut items {
            if item.state == DownloadState::InProgress {
                item.state = DownloadState::Failed;
            }
        }
        Self { items }
    }

    /// Saves the list, leaving out downloads made in private tabs.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let kept: Vec<&Download> = self.items.iter().filter(|d| !d.private).collect();
        write_atomic(path, &serde_json::to_string_pretty(&kept)?)
    }

    pub fn items(&self) -> &[Download] {
        &self.items
    }

    pub fn get(&self, id: DownloadId) -> Option<&Download> {
        self.items.iter().find(|d| d.id == id)
    }

    /// The first id that is safe to hand out for a new download.
    pub fn next_id(&self) -> DownloadId {
        self.items.iter().map(|d| d.id).max().map_or(1, |max| max + 1)
    }

    pub fn active_count(&self) -> usize {
        self.items.iter().filter(|d| d.state == DownloadState::InProgress).count()
    }

    /// Aggregate completion percentage (0..=100) across active downloads whose
    /// total size is known, or `None` when none have a known total.
    pub fn active_progress(&self) -> Option<u8> {
        let mut received = 0i128;
        let mut total = 0i128;
        for d in self.items.iter().filter(|d| d.state == DownloadState::InProgress && d.total > 0) {
            received += i128::from(d.received.max(0));
            total += i128::from(d.total);
        }
        (total > 0).then(|| ((received * 100) / total).clamp(0, 100) as u8)
    }

    pub fn start(&mut self, download: Download) {
        self.items.insert(0, download);
        // Trim the oldest finished entries; never drop one that is still running.
        let mut index = self.items.len();
        while self.items.len() > MAX_ITEMS && index > 0 {
            index -= 1;
            if self.items[index].state != DownloadState::InProgress {
                self.items.remove(index);
            }
        }
    }

    /// Returns `true` if the download exists and is still running.
    pub fn progress(&mut self, id: DownloadId, received: i64, total: i64) -> bool {
        match self.items.iter_mut().find(|d| d.id == id) {
            Some(d) if d.state == DownloadState::InProgress => {
                d.received = received;
                d.total = total;
                true
            }
            _ => false,
        }
    }

    pub fn finish(&mut self, id: DownloadId, state: DownloadState, path: Option<PathBuf>, received: i64) {
        if let Some(d) = self.items.iter_mut().find(|d| d.id == id) {
            d.state = state;
            d.received = received.max(d.received);
            if let Some(path) = path {
                d.path = path;
            }
        }
    }

    /// Removes a finished download from the list. Running downloads must be
    /// cancelled first. Returns `true` if something was removed.
    pub fn remove(&mut self, id: DownloadId) -> bool {
        let before = self.items.len();
        self.items.retain(|d| d.id != id || d.state == DownloadState::InProgress);
        self.items.len() != before
    }

    /// Removes every finished download from the list. Returns `true` if
    /// anything was removed.
    pub fn clear_finished(&mut self) -> bool {
        let before = self.items.len();
        self.items.retain(|d| d.state == DownloadState::InProgress);
        self.items.len() != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn download(id: DownloadId, state: DownloadState) -> Download {
        Download {
            id,
            url: format!("https://example.com/file{id}.zip"),
            path: PathBuf::from(format!(r"C:\Users\me\Downloads\file{id}.zip")),
            received: 0,
            total: 1000,
            state,
            private: false,
        }
    }

    #[test]
    fn private_downloads_are_listed_but_never_saved() {
        let path = std::env::temp_dir().join(format!("rust-browser-downloads-private-{}.json", std::process::id()));
        let mut list = Downloads::default();
        list.start(download(1, DownloadState::Completed));
        list.start(Download { private: true, ..download(2, DownloadState::Completed) });
        assert_eq!(list.items().len(), 2);

        list.save(&path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("file2.zip"), "private download leaked into {text}");
        let loaded = Downloads::load(&path);
        assert_eq!(loaded.items().iter().map(|d| d.id).collect::<Vec<_>>(), vec![1]);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn newest_download_is_first() {
        let mut list = Downloads::default();
        list.start(download(1, DownloadState::InProgress));
        list.start(download(2, DownloadState::InProgress));
        assert_eq!(list.items().iter().map(|d| d.id).collect::<Vec<_>>(), vec![2, 1]);
        assert_eq!(list.active_count(), 2);
        assert_eq!(list.next_id(), 3);
        assert_eq!(Downloads::default().next_id(), 1);
    }

    #[test]
    fn progress_then_finish() {
        let mut list = Downloads::default();
        assert_eq!(list.active_progress(), None);
        list.start(download(1, DownloadState::InProgress));
        assert!(list.progress(1, 400, 1000));
        assert_eq!(list.get(1).unwrap().received, 400);
        assert_eq!(list.active_progress(), Some(40));

        let final_path = PathBuf::from(r"C:\Users\me\Downloads\file1 (1).zip");
        list.finish(1, DownloadState::Completed, Some(final_path.clone()), 1000);
        let d = list.get(1).unwrap();
        assert_eq!((d.state, d.received, &d.path), (DownloadState::Completed, 1000, &final_path));
        assert_eq!(d.file_name(), "file1 (1).zip");
        assert_eq!(list.active_count(), 0);
        assert_eq!(list.active_progress(), None);

        // Late progress events for a finished download are ignored.
        assert!(!list.progress(1, 5, 1000));
        assert!(!list.progress(99, 5, 1000));
        assert_eq!(list.get(1).unwrap().received, 1000);
    }

    #[test]
    fn running_downloads_cannot_be_removed_or_cleared() {
        let mut list = Downloads::default();
        list.start(download(1, DownloadState::Completed));
        list.start(download(2, DownloadState::InProgress));
        list.start(download(3, DownloadState::Failed));

        assert!(!list.remove(2));
        assert!(list.remove(3));
        assert!(!list.remove(3));
        assert!(list.clear_finished());
        assert_eq!(list.items().iter().map(|d| d.id).collect::<Vec<_>>(), vec![2]);
        assert!(!list.clear_finished());
    }

    #[test]
    fn list_is_capped_but_keeps_running_downloads() {
        let mut list = Downloads::default();
        list.start(download(1, DownloadState::InProgress));
        for id in 2..=(MAX_ITEMS as DownloadId + 20) {
            list.start(download(id, DownloadState::Completed));
        }
        assert_eq!(list.items().len(), MAX_ITEMS);
        assert!(list.get(1).is_some(), "the running download must survive trimming");
        assert!(list.get(2).is_none(), "the oldest finished download is dropped");
    }

    #[test]
    fn save_and_load_marks_interrupted_downloads_failed() {
        let path = std::env::temp_dir().join(format!("rust-browser-downloads-test-{}.json", std::process::id()));
        let mut list = Downloads::default();
        list.start(download(1, DownloadState::Completed));
        list.start(download(2, DownloadState::InProgress));
        list.save(&path).unwrap();

        let loaded = Downloads::load(&path);
        assert_eq!(loaded.get(1).unwrap().state, DownloadState::Completed);
        assert_eq!(loaded.get(2).unwrap().state, DownloadState::Failed);
        assert_eq!(loaded.next_id(), 3);

        fs::write(&path, "garbage").unwrap();
        assert!(Downloads::load(&path).items().is_empty());
        let _ = fs::remove_file(&path);
        assert!(Downloads::load(&path).items().is_empty());
    }
}
