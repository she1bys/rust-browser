//! New Tab Page state, safe HTML rendering, and `rbnewtab` custom protocol handler.

use crate::bookmarks::Bookmarks;
use crate::history::History;
use crate::settings::Settings;
use serde::Serialize;
use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::RwLock;
use wry::http::{header, Method, Request, Response, StatusCode};

/// Canonical internal URL stored in tab state and session snapshots for a New Tab.
pub const NEW_TAB_URL: &str = "about:blank";
/// Custom protocol scheme registered on tab WebViews.
pub const SCHEME: &str = "rbnewtab";
/// HTTPS-mapped custom protocol URL for normal New Tab pages.
pub const NORMAL_URL: &str = "https://rbnewtab.localhost/";
/// HTTPS-mapped custom protocol URL for Private New Tab pages.
pub const PRIVATE_URL: &str = "https://rbnewtab.localhost/private";

const HTML: &str = include_str!("../ui/newtab.html");
const DATA_MARKER: &str = "/* __NTP_DATA__ */";
const NTP_ORIGIN: &str = "https://rbnewtab.localhost";
const MAX_QUICK_LINKS: usize = 8;
const MAX_RECENT_LINKS: usize = 4;

const CSP_HEADER: &str = "default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; img-src data:; connect-src 'none'; frame-ancestors 'none'; form-action https: http:";

/// A Quick Access or Jump Back In link displayed on the New Tab page.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct NewTabLink {
    pub url: String,
    pub title: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub bookmarked: bool,
}

/// Shared read-only snapshot used by the `rbnewtab` protocol handler and live updates.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NewTabState {
    pub engine_name: String,
    pub search_prefix: String,
    pub quick: Vec<NewTabLink>,
    pub recent: Vec<NewTabLink>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NewTabPayload<'a> {
    private: bool,
    engine_name: &'a str,
    search_prefix: &'a str,
    quick: &'a [NewTabLink],
    recent: &'a [NewTabLink],
}

impl NewTabState {
    /// Builds a sanitized New Tab snapshot from user settings, bookmarks, and history.
    pub fn from_sources(settings: &Settings, bookmarks: &Bookmarks, history: &History) -> Self {
        let mut seen = HashSet::new();
        let mut quick = Vec::with_capacity(MAX_QUICK_LINKS);

        for bookmark in bookmarks.items() {
            if quick.len() >= MAX_QUICK_LINKS {
                break;
            }
            let url = bookmark.url.trim();
            if is_safe_http_url(url) && seen.insert(url.to_string()) {
                quick.push(NewTabLink {
                    url: url.to_string(),
                    title: bookmark.title.trim().to_string(),
                    bookmarked: true,
                });
            }
        }

        let mut safe_history: Vec<_> =
            history.items().iter().filter(|entry| is_safe_http_url(entry.url.trim())).collect();

        if quick.len() < MAX_QUICK_LINKS {
            safe_history.sort_by_key(|a| std::cmp::Reverse((a.visits, a.last_visited)));
            for entry in &safe_history {
                if quick.len() >= MAX_QUICK_LINKS {
                    break;
                }
                let url = entry.url.trim();
                if seen.insert(url.to_string()) {
                    quick.push(NewTabLink {
                        url: url.to_string(),
                        title: entry.title.trim().to_string(),
                        bookmarked: false,
                    });
                }
            }
        }

        safe_history.sort_by_key(|a| std::cmp::Reverse(a.last_visited));
        let mut recent = Vec::with_capacity(MAX_RECENT_LINKS);
        for entry in safe_history {
            if recent.len() >= MAX_RECENT_LINKS {
                break;
            }
            let url = entry.url.trim();
            if seen.insert(url.to_string()) {
                recent.push(NewTabLink {
                    url: url.to_string(),
                    title: entry.title.trim().to_string(),
                    bookmarked: false,
                });
            }
        }

        Self {
            engine_name: settings.search_engine.name().to_string(),
            search_prefix: settings.search_engine.search_prefix().to_string(),
            quick,
            recent,
        }
    }
}

/// Returns `true` if `url` identifies the built-in New Tab page.
pub fn is_internal_url(url: &str) -> bool {
    let trimmed = url.trim();
    trimmed.eq_ignore_ascii_case("about:blank")
        || trimmed.eq_ignore_ascii_case("about:newtab")
        || starts_with_ignore_ascii_case(trimmed, "rbnewtab://")
        || matches_origin_prefix(trimmed, NTP_ORIGIN)
}

/// Maps internal New Tab URLs to the WebView `rbnewtab` URL, leaving external URLs intact.
pub fn webview_url(url: &str, private: bool) -> &str {
    if is_internal_url(url) {
        if private || url_requests_private(url) {
            PRIVATE_URL
        } else {
            NORMAL_URL
        }
    } else {
        url
    }
}

/// Normalizes any internal New Tab URL back to `about:blank` for tab/session/chrome state.
pub fn normalize_url(url: &str) -> String {
    if is_internal_url(url) {
        NEW_TAB_URL.to_string()
    } else {
        url.to_string()
    }
}

/// Renders the complete New Tab HTML document with safely escaped inline bootstrap JSON.
pub fn render_html(state: &NewTabState, private: bool) -> String {
    let payload_json = serialized_payload(state, private);
    if let Some((before, after)) = HTML.split_once(DATA_MARKER) {
        let mut out = String::with_capacity(before.len() + payload_json.len() + after.len());
        out.push_str(before);
        out.push_str(&payload_json);
        out.push_str(after);
        out
    } else {
        HTML.to_string()
    }
}

/// Builds a JS snippet to live-refresh an already-open normal New Tab page.
pub fn update_script(state: &NewTabState) -> String {
    let payload_json = serialized_payload(state, false);
    format!("window.__ntpApply && window.__ntpApply({payload_json});")
}

/// Handles `rbnewtab` custom protocol requests from a tab WebView.
pub fn handle_request(state: &RwLock<NewTabState>, req: Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
    if req.method() != Method::GET {
        return text_response(StatusCode::METHOD_NOT_ALLOWED, b"Method Not Allowed");
    }
    if !allowed_fetch_headers(&req) {
        return text_response(StatusCode::FORBIDDEN, b"Forbidden");
    }

    let private = match req.uri().path() {
        "" | "/" => false,
        "/private" => true,
        _ => return text_response(StatusCode::NOT_FOUND, b"Not Found"),
    };

    let html = match state.read() {
        Ok(guard) => render_html(&guard, private),
        Err(poisoned) => render_html(&poisoned.into_inner(), private),
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-store")
        .header("Referrer-Policy", "no-referrer")
        .header("X-Content-Type-Options", "nosniff")
        .header("X-Frame-Options", "DENY")
        .header("Content-Security-Policy", CSP_HEADER)
        .body(Cow::Owned(html.into_bytes()))
        .unwrap_or_else(|_| text_response(StatusCode::INTERNAL_SERVER_ERROR, b"Internal Error"))
}

fn serialized_payload(state: &NewTabState, private: bool) -> String {
    let payload = if private {
        NewTabPayload {
            private: true,
            engine_name: &state.engine_name,
            search_prefix: &state.search_prefix,
            quick: &[],
            recent: &[],
        }
    } else {
        NewTabPayload {
            private: false,
            engine_name: &state.engine_name,
            search_prefix: &state.search_prefix,
            quick: &state.quick,
            recent: &state.recent,
        }
    };
    let raw = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
    escape_json_for_inline_script(&raw)
}

fn escape_json_for_inline_script(json: &str) -> String {
    let mut escaped = String::with_capacity(json.len());
    for ch in json.chars() {
        match ch {
            '<' => escaped.push_str("\\u003c"),
            '>' => escaped.push_str("\\u003e"),
            '&' => escaped.push_str("\\u0026"),
            '\u{2028}' => escaped.push_str("\\u2028"),
            '\u{2029}' => escaped.push_str("\\u2029"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

fn allowed_fetch_headers(req: &Request<Vec<u8>>) -> bool {
    if let Some(site) = req.headers().get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
        let site = site.trim();
        if !site.eq_ignore_ascii_case("none") && !site.eq_ignore_ascii_case("same-origin") {
            return false;
        }
    }
    if let Some(origin) = req.headers().get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        if !origin.trim().eq_ignore_ascii_case(NTP_ORIGIN) {
            return false;
        }
    }
    true
}

fn text_response(status: StatusCode, body: &'static [u8]) -> Response<Cow<'static, [u8]>> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .header("X-Content-Type-Options", "nosniff")
        .body(Cow::Borrowed(body))
        .expect("static error response should always build")
}

fn is_safe_http_url(url: &str) -> bool {
    starts_with_ignore_ascii_case(url, "http://") || starts_with_ignore_ascii_case(url, "https://")
}

fn starts_with_ignore_ascii_case(text: &str, prefix: &str) -> bool {
    text.get(..prefix.len()).is_some_and(|head| head.eq_ignore_ascii_case(prefix))
}

fn matches_origin_prefix(url: &str, origin: &str) -> bool {
    if !starts_with_ignore_ascii_case(url, origin) {
        return false;
    }
    matches!(url.as_bytes().get(origin.len()), None | Some(b'/' | b'?' | b'#'))
}

fn url_requests_private(url: &str) -> bool {
    let trimmed = url.trim();
    trimmed.eq_ignore_ascii_case(PRIVATE_URL) || trimmed.eq_ignore_ascii_case("rbnewtab://localhost/private")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::SearchEngine;

    #[test]
    fn recognizes_and_normalizes_internal_urls() {
        assert!(is_internal_url("about:blank"));
        assert!(is_internal_url("ABOUT:NEWTAB"));
        assert!(is_internal_url("https://rbnewtab.localhost/"));
        assert!(is_internal_url("https://rbnewtab.localhost/private"));
        assert!(is_internal_url("rbnewtab://localhost/"));
        assert!(!is_internal_url("https://rbnewtab.localhost.example.com/"));
        assert!(!is_internal_url("https://example.com/"));

        assert_eq!(webview_url("about:blank", false), NORMAL_URL);
        assert_eq!(webview_url("about:blank", true), PRIVATE_URL);
        assert_eq!(webview_url("https://example.com/", false), "https://example.com/");
        assert_eq!(normalize_url(PRIVATE_URL), NEW_TAB_URL);
        assert_eq!(normalize_url("https://example.com/"), "https://example.com/");
    }

    #[test]
    fn builds_deduped_quick_and_recent_links_filtering_unsafe_schemes() {
        let settings = Settings { search_engine: SearchEngine::DuckDuckGo, ..Default::default() };

        let mut bookmarks = Bookmarks::default();
        bookmarks.toggle("javascript:alert(1)".into(), "Bad".into());
        bookmarks.toggle("https://rust-lang.org/".into(), "Rust".into());

        let mut history = History::default();
        history.record("https://rust-lang.org/", "Rust Lang");
        history.record("https://docs.rs/", "Docs.rs");
        history.record("https://docs.rs/", "Docs.rs");
        history.record("file:///C:/secret.txt", "Secret");

        let state = NewTabState::from_sources(&settings, &bookmarks, &history);
        assert_eq!(state.engine_name, "DuckDuckGo");
        assert_eq!(state.quick.len(), 2);
        assert_eq!(state.quick[0].url, "https://rust-lang.org/");
        assert!(state.quick[0].bookmarked);
        assert_eq!(state.quick[1].url, "https://docs.rs/");
        assert!(!state.quick[1].bookmarked);
        assert!(state.recent.is_empty());
    }

    #[test]
    fn escapes_script_breaking_json_and_strips_private_links() {
        let state = NewTabState {
            engine_name: "Google".into(),
            search_prefix: "https://www.google.com/search?q=".into(),
            quick: vec![NewTabLink {
                url: "https://example.com/?a=1&b=2".into(),
                title: "</script><script>alert(1)</script>".into(),
                bookmarked: true,
            }],
            recent: vec![],
        };

        let normal = render_html(&state, false);
        assert!(normal.contains("\\u003c/script\\u003e"));
        assert!(normal.contains("example.com"));

        let private = render_html(&state, true);
        assert!(!private.contains("example.com"));
        assert!(private.contains("\"private\":true"));
    }

    #[test]
    fn protocol_handler_enforces_paths_methods_and_origin() {
        let state = RwLock::new(NewTabState {
            engine_name: "Google".into(),
            search_prefix: "https://www.google.com/search?q=".into(),
            quick: vec![NewTabLink { url: "https://example.com/".into(), title: "Example".into(), bookmarked: false }],
            recent: vec![],
        });

        let ok = handle_request(&state, Request::get("rbnewtab://localhost/").body(Vec::new()).unwrap());
        assert_eq!(ok.status(), StatusCode::OK);
        assert!(String::from_utf8_lossy(ok.body()).contains("https://example.com/"));

        let priv_ok = handle_request(&state, Request::get("rbnewtab://localhost/private").body(Vec::new()).unwrap());
        assert_eq!(priv_ok.status(), StatusCode::OK);
        assert!(!String::from_utf8_lossy(priv_ok.body()).contains("https://example.com/"));

        let bad_path = handle_request(&state, Request::get("rbnewtab://localhost/other").body(Vec::new()).unwrap());
        assert_eq!(bad_path.status(), StatusCode::NOT_FOUND);

        let cross_site = handle_request(
            &state,
            Request::get("rbnewtab://localhost/").header("sec-fetch-site", "cross-site").body(Vec::new()).unwrap(),
        );
        assert_eq!(cross_site.status(), StatusCode::FORBIDDEN);

        let foreign_origin = handle_request(
            &state,
            Request::get("rbnewtab://localhost/").header("origin", "https://evil.example").body(Vec::new()).unwrap(),
        );
        assert_eq!(foreign_origin.status(), StatusCode::FORBIDDEN);
    }
}
