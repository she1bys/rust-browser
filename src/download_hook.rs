//! Connects WebView2's download events to the browser.
//!
//! wry only reports "started" and "finished", so this talks to WebView2
//! directly to also get byte-by-byte progress and the ability to cancel.
//! Everything here runs on the UI thread (WebView2 calls back on the thread
//! that created the web view), which is why plain `Rc`/`RefCell` are enough.

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    ffi::OsStr,
    path::{Path, PathBuf},
    rc::Rc,
};

use tao::event_loop::EventLoopProxy;
use webview2_com::{
    take_pwstr, BytesReceivedChangedEventHandler, DownloadStartingEventHandler,
    Microsoft::Web::WebView2::Win32::{
        ICoreWebView2DownloadOperation, ICoreWebView2_4, COREWEBVIEW2_DOWNLOAD_STATE,
        COREWEBVIEW2_DOWNLOAD_STATE_COMPLETED, COREWEBVIEW2_DOWNLOAD_STATE_IN_PROGRESS,
    },
    StateChangedEventHandler,
};
use windows::core::{Interface, HSTRING, PWSTR};
use wry::{WebView, WebViewExtWindows};

use crate::ipc::{DownloadId, TabId, UserEvent};

type Operations = Rc<RefCell<HashMap<DownloadId, ICoreWebView2DownloadOperation>>>;

pub struct DownloadHook {
    next_id: Rc<Cell<DownloadId>>,
    /// Downloads that are still running, so they can be cancelled.
    operations: Operations,
    proxy: EventLoopProxy<UserEvent>,
    /// Folder chosen in Settings; `None` leaves WebView2's default (Downloads).
    folder: Rc<RefCell<Option<PathBuf>>>,
    /// Files that running downloads are going to create, so two downloads
    /// of the same name can't be given the same path.
    reserved: Rc<RefCell<HashSet<PathBuf>>>,
}

impl DownloadHook {
    pub fn new(first_id: DownloadId, proxy: EventLoopProxy<UserEvent>, folder: Option<PathBuf>) -> Self {
        Self {
            next_id: Rc::new(Cell::new(first_id)),
            operations: Rc::new(RefCell::new(HashMap::new())),
            proxy,
            folder: Rc::new(RefCell::new(folder)),
            reserved: Rc::new(RefCell::new(HashSet::new())),
        }
    }

    /// Changes where new downloads are saved. Running ones are not moved.
    pub fn set_folder(&self, folder: Option<PathBuf>) {
        *self.folder.borrow_mut() = folder;
    }

    /// Starts reporting downloads that begin in `webview` (the page of tab `tab`).
    pub fn attach(&self, webview: &WebView, tab: TabId) -> windows::core::Result<()> {
        let core: ICoreWebView2_4 = webview.webview().cast()?;
        let next_id = self.next_id.clone();
        let operations = self.operations.clone();
        let proxy = self.proxy.clone();
        let folder = self.folder.clone();
        let reserved = self.reserved.clone();

        let handler = DownloadStartingEventHandler::create(Box::new(move |_, args| {
            let Some(args) = args else { return Ok(()) };
            // SAFETY: plain COM calls on live objects WebView2 just handed us,
            // on the thread that owns them; out-pointers are valid locals.
            unsafe {
                let operation = args.DownloadOperation()?;

                let mut text = PWSTR::null();
                operation.Uri(&mut text)?;
                let url = take_pwstr(text);

                let mut text = PWSTR::null();
                args.ResultFilePath(&mut text)?;
                let mut path = PathBuf::from(take_pwstr(text));

                // Save into the folder chosen in Settings, if it still exists;
                // otherwise WebView2's own choice (the Downloads folder) stands.
                let chosen = folder.borrow().clone().filter(|dir| dir.is_dir());
                if let (Some(dir), Some(name)) = (chosen, path.file_name()) {
                    let target = unique_path(&dir, name, &reserved.borrow());
                    args.SetResultFilePath(&HSTRING::from(target.as_path()))?;
                    path = target;
                }
                reserved.borrow_mut().insert(path.clone());

                let mut total = 0i64;
                operation.TotalBytesToReceive(&mut total)?;

                let id = next_id.get();
                next_id.set(id + 1);

                // The browser has its own Downloads panel, so hide WebView2's popup.
                args.SetHandled(true)?;

                let progress_proxy = proxy.clone();
                let mut token = 0i64;
                operation.add_BytesReceivedChanged(
                    &BytesReceivedChangedEventHandler::create(Box::new(move |operation, _| {
                        let Some(operation) = operation else { return Ok(()) };
                        let (mut received, mut total) = (0i64, 0i64);
                        operation.BytesReceived(&mut received)?;
                        operation.TotalBytesToReceive(&mut total)?;
                        let _ = progress_proxy.send_event(UserEvent::DownloadProgress { id, received, total });
                        Ok(())
                    })),
                    &mut token,
                )?;

                let finished_proxy = proxy.clone();
                let finished_operations = operations.clone();
                let finished_reserved = reserved.clone();
                let reserved_path = path.clone();
                operation.add_StateChanged(
                    &StateChangedEventHandler::create(Box::new(move |operation, _| {
                        let Some(operation) = operation else { return Ok(()) };
                        let mut state = COREWEBVIEW2_DOWNLOAD_STATE::default();
                        operation.State(&mut state)?;
                        if state == COREWEBVIEW2_DOWNLOAD_STATE_IN_PROGRESS {
                            return Ok(());
                        }
                        let completed = state == COREWEBVIEW2_DOWNLOAD_STATE_COMPLETED;
                        let path = if completed {
                            let mut text = PWSTR::null();
                            operation.ResultFilePath(&mut text)?;
                            Some(PathBuf::from(take_pwstr(text)))
                        } else {
                            None
                        };
                        let mut received = 0i64;
                        operation.BytesReceived(&mut received)?;

                        finished_operations.borrow_mut().remove(&id);
                        finished_reserved.borrow_mut().remove(&reserved_path);
                        let _ =
                            finished_proxy.send_event(UserEvent::DownloadFinished { id, completed, path, received });
                        Ok(())
                    })),
                    &mut token,
                )?;

                operations.borrow_mut().insert(id, operation);
                let _ = proxy.send_event(UserEvent::DownloadStarted { id, tab, url, path, total });
            }
            Ok(())
        }));

        let mut token = 0i64;
        // SAFETY: `core` is a live web view and `token` is a valid out-pointer.
        unsafe { core.add_DownloadStarting(&handler, &mut token) }
    }

    /// Cancels a running download. Returns `false` if it isn't running any more.
    pub fn cancel(&self, id: DownloadId) -> bool {
        // Clone the operation out first: `Cancel` can fire the state-changed
        // handler immediately, and that handler needs to borrow the map.
        let operation = self.operations.borrow().get(&id).cloned();
        match operation {
            // SAFETY: a live COM object owned by this thread.
            Some(operation) => unsafe { operation.Cancel().is_ok() },
            None => false,
        }
    }
}

/// Picks a path in `dir` for a file called `name` that doesn't collide with
/// an existing file or a download still in flight: `name`, then
/// `name (1)`, `name (2)`, ... with the number before the extension.
fn unique_path(dir: &Path, name: &OsStr, reserved: &HashSet<PathBuf>) -> PathBuf {
    let taken = |path: &Path| path.exists() || reserved.contains(path);
    let first = dir.join(name);
    if !taken(&first) {
        return first;
    }
    let file = Path::new(name);
    let stem = file.file_stem().unwrap_or(name).to_string_lossy().into_owned();
    let extension = file.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    (1u32..).map(|n| dir.join(format!("{stem} ({n}){extension}"))).find(|candidate| !taken(candidate)).unwrap_or(first)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn unique_path_avoids_existing_and_reserved_files() {
        let dir = std::env::temp_dir().join(format!("rust-browser-unique-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let mut reserved = HashSet::new();

        // Nothing in the way: the name is used as is.
        assert_eq!(unique_path(&dir, OsStr::new("report.pdf"), &reserved), dir.join("report.pdf"));

        // A file on disk is in the way.
        fs::write(dir.join("report.pdf"), b"x").unwrap();
        assert_eq!(unique_path(&dir, OsStr::new("report.pdf"), &reserved), dir.join("report (1).pdf"));

        // So is a download that is still running and hasn't created its file yet.
        reserved.insert(dir.join("report (1).pdf"));
        assert_eq!(unique_path(&dir, OsStr::new("report.pdf"), &reserved), dir.join("report (2).pdf"));

        // Names without an extension, and with several dots.
        fs::write(dir.join("LICENSE"), b"x").unwrap();
        assert_eq!(unique_path(&dir, OsStr::new("LICENSE"), &reserved), dir.join("LICENSE (1)"));
        fs::write(dir.join("data.tar.gz"), b"x").unwrap();
        assert_eq!(unique_path(&dir, OsStr::new("data.tar.gz"), &reserved), dir.join("data.tar (1).gz"));

        let _ = fs::remove_dir_all(&dir);
    }
}
