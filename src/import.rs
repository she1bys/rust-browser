//! Import bookmarks and history from Chrome, Edge or Brave.
//!
//! Bookmarks come from the browser's `Bookmarks` file (JSON); history from its
//! `History` database (SQLite), read with the SQLite engine that ships with
//! Windows (`winsqlite3.dll`), so no database library is added to the browser.
//! The database is copied first, so this works while the other browser runs.
//!
//! Saved passwords are not imported: WebView2 has no way for an app to add
//! passwords to its password store, and reading them out of another browser
//! would mean decrypting its protected password store.

use std::{
    env,
    ffi::{c_char, c_int, c_void, CString},
    fs,
    path::{Path, PathBuf},
    ptr,
};

use serde::Serialize;
use serde_json::Value;

use crate::bookmarks::Bookmark;
use crate::history::HistoryEntry;

/// The most history entries imported (the browser keeps 500).
const HISTORY_LIMIT: usize = 500;

/// A browser to import from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Chrome,
    Edge,
    Brave,
}

impl Source {
    pub const ALL: [Source; 3] = [Source::Chrome, Source::Edge, Source::Brave];

    pub fn name(self) -> &'static str {
        match self {
            Source::Chrome => "Google Chrome",
            Source::Edge => "Microsoft Edge",
            Source::Brave => "Brave",
        }
    }

    /// The browser's main ("Default") profile folder.
    fn profile_dir(self) -> Option<PathBuf> {
        let base = PathBuf::from(env::var_os("LOCALAPPDATA")?);
        let user_data = match self {
            Source::Chrome => base.join(r"Google\Chrome\User Data"),
            Source::Edge => base.join(r"Microsoft\Edge\User Data"),
            Source::Brave => base.join(r"BraveSoftware\Brave-Browser\User Data"),
        };
        Some(user_data.join("Default"))
    }

    /// The browser is installed and has been used on this PC.
    pub fn is_available(self) -> bool {
        self.profile_dir().is_some_and(|dir| dir.join("Bookmarks").is_file() || dir.join("History").is_file())
    }
}

/// What was read from another browser.
#[derive(Debug, Default)]
pub struct Imported {
    pub bookmarks: Vec<Bookmark>,
    pub history: Vec<HistoryEntry>,
}

/// Reads `source`'s bookmarks and history. Either may be missing; an error
/// only if neither could be read.
pub fn import(source: Source) -> Result<Imported, String> {
    let dir = source.profile_dir().ok_or_else(|| format!("{} wasn't found on this PC", source.name()))?;
    let bookmarks = read_bookmarks(&dir.join("Bookmarks"));
    let history = read_history(&dir.join("History"));
    match (bookmarks, history) {
        (Err(b), Err(h)) => Err(format!("couldn't read {}'s data ({b}; {h})", source.name())),
        (bookmarks, history) => {
            Ok(Imported { bookmarks: bookmarks.unwrap_or_default(), history: history.unwrap_or_default() })
        }
    }
}

// ---- bookmarks ---------------------------------------------------------------

fn read_bookmarks(path: &Path) -> Result<Vec<Bookmark>, String> {
    let text = fs::read_to_string(path).map_err(|_| "no bookmarks file".to_string())?;
    parse_bookmarks(&text)
}

/// Every bookmark in a Chromium `Bookmarks` file (bookmarks bar first, then
/// other and mobile bookmarks), in order, folders flattened.
fn parse_bookmarks(json: &str) -> Result<Vec<Bookmark>, String> {
    let root: Value = serde_json::from_str(json).map_err(|_| "the bookmarks file is damaged".to_string())?;
    let mut out = Vec::new();
    for key in ["bookmark_bar", "other", "synced"] {
        if let Some(node) = root.pointer(&format!("/roots/{key}")) {
            collect_bookmarks(node, &mut out);
        }
    }
    Ok(out)
}

fn collect_bookmarks(node: &Value, out: &mut Vec<Bookmark>) {
    match node.get("type").and_then(Value::as_str) {
        Some("url") => {
            let url = node.get("url").and_then(Value::as_str).unwrap_or_default();
            if is_importable(url) {
                let name = node.get("name").and_then(Value::as_str).unwrap_or_default().trim();
                out.push(Bookmark {
                    url: url.to_string(),
                    title: if name.is_empty() { url } else { name }.to_string(),
                });
            }
        }
        _ => {
            for child in node.get("children").and_then(Value::as_array).into_iter().flatten() {
                collect_bookmarks(child, out);
            }
        }
    }
}

/// Only web pages (no `javascript:` bookmarklets or browser-internal pages).
fn is_importable(url: &str) -> bool {
    let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    lower.starts_with("https://") || lower.starts_with("http://")
}

// ---- history -----------------------------------------------------------------

fn read_history(path: &Path) -> Result<Vec<HistoryEntry>, String> {
    if !path.is_file() {
        return Err("no history file".into());
    }
    // Read a copy: the other browser may have the database open.
    let copy = env::temp_dir().join(format!("rust-browser-import-{}.db", std::process::id()));
    fs::copy(path, &copy).map_err(|_| "its history is in use; close it and try again".to_string())?;
    type Row = (String, String, i64, i64);
    let result = (|| -> Result<Vec<Row>, String> {
        let db = sqlite::Database::open_read_only(&copy)?;
        let mut rows = Vec::new();
        db.query(
            "SELECT url, title, visit_count, last_visit_time FROM urls \
             WHERE hidden = 0 AND last_visit_time > 0 ORDER BY last_visit_time DESC LIMIT 2000",
            |row| rows.push((row.text(0), row.text(1), row.int(2), row.int(3))),
        )?;
        Ok(rows)
    })();
    let _ = fs::remove_file(&copy);
    let rows = result?;
    Ok(rows
        .into_iter()
        .filter(|(url, ..)| is_importable(url))
        .map(|(url, title, visits, chrome_time)| HistoryEntry {
            title: if title.trim().is_empty() { url.clone() } else { title },
            url,
            visits: visits.clamp(1, i64::from(u32::MAX)) as u32,
            last_visited: unix_seconds_from_chrome_time(chrome_time),
        })
        .take(HISTORY_LIMIT)
        .collect())
}

/// Chromium stores times as microseconds since 1 January 1601 (UTC).
fn unix_seconds_from_chrome_time(micros: i64) -> u64 {
    const SECONDS_1601_TO_1970: i64 = 11_644_473_600;
    (micros / 1_000_000 - SECONDS_1601_TO_1970).max(0) as u64
}

/// The few SQLite calls needed, from Windows' own `winsqlite3.dll`.
mod sqlite {
    use super::*;
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32};

    const SQLITE_OK: c_int = 0;
    const SQLITE_ROW: c_int = 100;
    const SQLITE_DONE: c_int = 101;
    const SQLITE_OPEN_READONLY: c_int = 0x1;

    type Open = unsafe extern "system" fn(*const c_char, *mut *mut c_void, c_int, *const c_char) -> c_int;
    type Prepare =
        unsafe extern "system" fn(*mut c_void, *const c_char, c_int, *mut *mut c_void, *mut *const c_char) -> c_int;
    type Step = unsafe extern "system" fn(*mut c_void) -> c_int;
    type ColumnText = unsafe extern "system" fn(*mut c_void, c_int) -> *const u8;
    type ColumnBytes = unsafe extern "system" fn(*mut c_void, c_int) -> c_int;
    type ColumnInt64 = unsafe extern "system" fn(*mut c_void, c_int) -> i64;
    type Finalize = unsafe extern "system" fn(*mut c_void) -> c_int;
    type Close = unsafe extern "system" fn(*mut c_void) -> c_int;

    struct Api {
        open: Open,
        prepare: Prepare,
        step: Step,
        column_text: ColumnText,
        column_bytes: ColumnBytes,
        column_int64: ColumnInt64,
        finalize: Finalize,
        close: Close,
    }

    fn api() -> Result<Api, String> {
        let missing = || "Windows' database engine (winsqlite3.dll) isn't available".to_string();
        let name: Vec<u16> = "winsqlite3.dll\0".encode_utf16().collect();
        // SAFETY: loads a system DLL by name from System32 only; the function
        // pointers are transmuted to their documented SQLite C signatures.
        unsafe {
            let module = LoadLibraryExW(name.as_ptr(), ptr::null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32);
            if module.is_null() {
                return Err(missing());
            }
            type Farproc = unsafe extern "system" fn() -> isize;
            macro_rules! function {
                ($name:literal, $ty:ty) => {
                    std::mem::transmute::<Farproc, $ty>(
                        GetProcAddress(module, concat!($name, "\0").as_ptr()).ok_or_else(missing)?,
                    )
                };
            }
            Ok(Api {
                open: function!("sqlite3_open_v2", Open),
                prepare: function!("sqlite3_prepare_v2", Prepare),
                step: function!("sqlite3_step", Step),
                column_text: function!("sqlite3_column_text", ColumnText),
                column_bytes: function!("sqlite3_column_bytes", ColumnBytes),
                column_int64: function!("sqlite3_column_int64", ColumnInt64),
                finalize: function!("sqlite3_finalize", Finalize),
                close: function!("sqlite3_close", Close),
            })
        }
    }

    pub struct Database {
        api: Api,
        handle: *mut c_void,
    }

    pub struct Row<'a> {
        api: &'a Api,
        statement: *mut c_void,
    }

    impl Row<'_> {
        pub fn text(&self, column: c_int) -> String {
            // SAFETY: `statement` is on a row; SQLite owns the returned buffer
            // until the next step, and `column_bytes` gives its length.
            unsafe {
                let text = (self.api.column_text)(self.statement, column);
                let len = (self.api.column_bytes)(self.statement, column);
                if text.is_null() || len <= 0 {
                    return String::new();
                }
                String::from_utf8_lossy(std::slice::from_raw_parts(text, len as usize)).into_owned()
            }
        }

        pub fn int(&self, column: c_int) -> i64 {
            // SAFETY: `statement` is on a row.
            unsafe { (self.api.column_int64)(self.statement, column) }
        }
    }

    impl Database {
        pub fn open_read_only(path: &Path) -> Result<Database, String> {
            let api = api()?;
            let path = CString::new(path.to_string_lossy().as_bytes()).map_err(|_| "bad path".to_string())?;
            let mut handle = ptr::null_mut();
            // SAFETY: valid NUL-terminated UTF-8 path and out-pointer.
            let rc = unsafe { (api.open)(path.as_ptr(), &mut handle, SQLITE_OPEN_READONLY, ptr::null()) };
            let db = Database { api, handle };
            if rc != SQLITE_OK {
                return Err("its history database couldn't be opened".into());
            }
            Ok(db)
        }

        /// Runs `sql`, calling `on_row` for every row.
        pub fn query(&self, sql: &str, mut on_row: impl FnMut(&Row)) -> Result<(), String> {
            let sql = CString::new(sql).map_err(|_| "bad query".to_string())?;
            let mut statement = ptr::null_mut();
            // SAFETY: open database, NUL-terminated SQL, valid out-pointer; the
            // statement is finalized before returning.
            unsafe {
                if (self.api.prepare)(self.handle, sql.as_ptr(), -1, &mut statement, ptr::null_mut()) != SQLITE_OK {
                    return Err("its history database has an unexpected format".into());
                }
                let result = loop {
                    match (self.api.step)(statement) {
                        SQLITE_ROW => on_row(&Row { api: &self.api, statement }),
                        SQLITE_DONE => break Ok(()),
                        _ => break Err("its history database couldn't be read".to_string()),
                    }
                };
                (self.api.finalize)(statement);
                result
            }
        }
    }

    impl Drop for Database {
        fn drop(&mut self) {
            // SAFETY: closing the handle we opened (null is a no-op for SQLite).
            unsafe { (self.api.close)(self.handle) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flattens_chromium_bookmarks() {
        let json = r#"{"roots":{
            "bookmark_bar":{"type":"folder","children":[
                {"type":"url","name":"Rust","url":"https://www.rust-lang.org/"},
                {"type":"folder","name":"Work","children":[
                    {"type":"url","name":"","url":"https://example.com/"},
                    {"type":"url","name":"Tool","url":"javascript:alert(1)"}
                ]}
            ]},
            "other":{"type":"folder","children":[{"type":"url","name":"Docs","url":"https://docs.rs/"}]},
            "synced":{"type":"folder","children":[]}
        },"version":1}"#;
        let bookmarks = parse_bookmarks(json).unwrap();
        let urls: Vec<&str> = bookmarks.iter().map(|b| b.url.as_str()).collect();
        assert_eq!(urls, ["https://www.rust-lang.org/", "https://example.com/", "https://docs.rs/"]);
        assert_eq!(bookmarks[1].title, "https://example.com/", "untitled bookmarks use their address");
        assert!(parse_bookmarks("{not json").is_err());
    }

    #[test]
    fn converts_chromium_times() {
        // 2024-01-01T00:00:00Z
        assert_eq!(unix_seconds_from_chrome_time(13_348_540_800_000_000), 1_704_067_200);
        assert_eq!(unix_seconds_from_chrome_time(0), 0);
    }

    #[test]
    fn reads_an_sqlite_history_database() {
        // Builds a small database with the same table as Chromium's, using
        // Windows' SQLite (read-write just for the test), then imports from it.
        let dir = env::temp_dir().join(format!("rust-browser-import-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("History");
        let _ = fs::remove_file(&path);
        write_test_history(&path);
        let entries = read_history(&path).unwrap();
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(entries.len(), 2, "hidden and non-web rows are skipped");
        assert_eq!(entries[0].url, "https://new.example/");
        assert_eq!(entries[0].visits, 7);
        assert_eq!(entries[0].last_visited, 1_704_067_200);
        assert_eq!(entries[1].title, "https://old.example/", "untitled rows use their address");
    }

    /// Creates a Chromium-like `urls` table with `sqlite3_exec`.
    fn write_test_history(path: &Path) {
        use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32};
        type Open = unsafe extern "system" fn(*const c_char, *mut *mut c_void, c_int, *const c_char) -> c_int;
        type Exec = unsafe extern "system" fn(
            *mut c_void,
            *const c_char,
            *const c_void,
            *mut c_void,
            *mut *mut c_char,
        ) -> c_int;
        type Close = unsafe extern "system" fn(*mut c_void) -> c_int;
        let name: Vec<u16> = "winsqlite3.dll\0".encode_utf16().collect();
        unsafe {
            let module = LoadLibraryExW(name.as_ptr(), ptr::null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32);
            type Farproc = unsafe extern "system" fn() -> isize;
            let find = |name: &std::ffi::CStr| GetProcAddress(module, name.as_ptr().cast()).unwrap();
            let open = std::mem::transmute::<Farproc, Open>(find(c"sqlite3_open_v2"));
            let exec = std::mem::transmute::<Farproc, Exec>(find(c"sqlite3_exec"));
            let close = std::mem::transmute::<Farproc, Close>(find(c"sqlite3_close"));
            let file = CString::new(path.to_string_lossy().as_bytes()).unwrap();
            let mut db = ptr::null_mut();
            assert_eq!(open(file.as_ptr(), &mut db, 0x2 | 0x4, ptr::null()), 0); // read-write, create
            let sql = c"CREATE TABLE urls(id INTEGER PRIMARY KEY, url TEXT, title TEXT, visit_count INTEGER, typed_count INTEGER, last_visit_time INTEGER, hidden INTEGER);
                INSERT INTO urls(url,title,visit_count,last_visit_time,hidden) VALUES
                ('https://old.example/','',1,13300000000000000,0),
                ('https://new.example/','New',7,13348540800000000,0),
                ('https://hidden.example/','Hidden',3,13348540900000000,1),
                ('chrome://settings/','Settings',2,13348541000000000,0);";
            assert_eq!(exec(db, sql.as_ptr(), ptr::null(), ptr::null_mut(), ptr::null_mut()), 0);
            close(db);
        }
    }
}
