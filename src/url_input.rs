//! Turns whatever the user typed into the address bar into a URL to load.
//!
//! Rules, in order:
//! 1. Empty input does nothing.
//! 2. A Windows path like `C:\docs\page.html` becomes a `file:///` URL.
//! 3. Input that already starts with a safe scheme (`http:`, `https:`, `file:`,
//!    `about:`, `view-source:`) is loaded as-is.
//! 4. Anything with spaces is a search.
//! 5. `localhost`, IPv4 and `[IPv6]` hosts get `http://`.
//! 6. Something that looks like a domain (`example.com/path`) gets `https://`.
//! 7. Everything else, including `javascript:` and unknown schemes, is a search
//!    with the search engine chosen in Settings.

const PASSTHROUGH_SCHEMES: [&str; 5] = ["http://", "https://", "file:", "about:", "view-source:"];

/// `search_prefix` is the chosen search engine's URL up to and including
/// `q=` (see `SearchEngine::search_prefix`); the encoded query is appended.
pub fn resolve(input: &str, search_prefix: &str) -> Option<String> {
    let search_url = |query: &str| search_url(query, search_prefix);

    let s = input.trim();
    if s.is_empty() {
        return None;
    }

    if let Some(url) = windows_path_to_url(s) {
        return Some(url);
    }

    let lower = s.to_ascii_lowercase();
    if PASSTHROUGH_SCHEMES.iter().any(|p| lower.starts_with(p)) {
        return Some(s.to_string());
    }

    if s.chars().any(char::is_whitespace) {
        return Some(search_url(s));
    }

    let host_port = s.split(['/', '?', '#']).next().unwrap_or("");

    // `[::1]:8080` style IPv6 literal.
    if host_port.starts_with('[') && host_port.contains(']') {
        return Some(format!("http://{s}"));
    }

    let host = match host_port.rsplit_once(':') {
        Some((host, port)) if is_port(port) => host,
        // A colon followed by something that isn't a port, e.g. `javascript:alert(1)`
        // or `mailto:x` - never navigate to those from the address bar.
        Some(_) => return Some(search_url(s)),
        None => host_port,
    };

    if host.eq_ignore_ascii_case("localhost") || is_ipv4(host) {
        return Some(format!("http://{s}"));
    }

    if is_domain(host) {
        return Some(format!("https://{s}"));
    }

    Some(search_url(s))
}

pub fn search_url(query: &str, search_prefix: &str) -> String {
    format!("{search_prefix}{}", encode_query(query))
}

/// `application/x-www-form-urlencoded` encoding: spaces become `+`,
/// unreserved characters stay, everything else is `%XX` of its UTF-8 bytes.
pub fn encode_query(query: &str) -> String {
    let mut out = String::with_capacity(query.len() * 3);
    for b in query.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn encode_file_path(path: &str) -> String {
    path.chars()
        .map(|c| match c {
            ' ' => "%20".to_string(),
            '#' => "%23".to_string(),
            '?' => "%3F".to_string(),
            '%' => "%25".to_string(),
            c => c.to_string(),
        })
        .collect()
}

fn windows_path_to_url(s: &str) -> Option<String> {
    if let Some(unc) = s.strip_prefix("\\\\") {
        // UNC path like `\\server\share\page.html` -> `file://server/share/page.html`.
        let forward = unc.replace('\\', "/");
        if forward.contains('/') && !forward.starts_with('/') {
            return Some(format!("file://{}", encode_file_path(&forward)));
        }
        return None;
    }

    let bytes = s.as_bytes();
    let is_drive_path = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/');
    if !is_drive_path {
        return None;
    }
    let path = s.replace('\\', "/");
    Some(format!("file:///{}", encode_file_path(&path)))
}

fn is_port(s: &str) -> bool {
    !s.is_empty() && s.len() <= 5 && s.bytes().all(|b| b.is_ascii_digit()) && s.parse::<u32>().is_ok_and(|p| p <= 65535)
}

fn is_ipv4(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    parts.len() == 4
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.len() <= 3
                && p.bytes().all(|b| b.is_ascii_digit())
                && p.parse::<u16>().is_ok_and(|n| n <= 255)
        })
}

fn is_domain(s: &str) -> bool {
    let labels: Vec<&str> = s.split('.').collect();
    if labels.len() < 2 {
        return false;
    }
    let valid_label = |l: &&str| {
        !l.is_empty()
            && l.len() <= 63
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.chars().all(|c| c.is_alphanumeric() || c == '-')
    };
    if !labels.iter().all(valid_label) {
        return false;
    }
    let tld = labels[labels.len() - 1];
    let tld_ok =
        tld.len() >= 2 && (tld.chars().all(char::is_alphabetic) || tld.to_ascii_lowercase().starts_with("xn--"));
    tld_ok
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEARCH_URL: &str = "https://search.example/?q=";

    fn r(s: &str) -> String {
        resolve(s, SEARCH_URL).expect("expected a URL")
    }

    #[test]
    fn empty_input_does_nothing() {
        assert_eq!(resolve("", SEARCH_URL), None);
        assert_eq!(resolve("   \t ", SEARCH_URL), None);
    }

    #[test]
    fn searches_use_the_chosen_engine() {
        assert_eq!(
            resolve("rust browser", "https://www.google.com/search?q=").unwrap(),
            "https://www.google.com/search?q=rust+browser"
        );
        assert_eq!(
            resolve("rust browser", "https://www.bing.com/search?q=").unwrap(),
            "https://www.bing.com/search?q=rust+browser"
        );
    }

    #[test]
    fn bare_domains_get_https() {
        assert_eq!(r("example.com"), "https://example.com");
        assert_eq!(r("  rust-lang.org/learn  "), "https://rust-lang.org/learn");
        assert_eq!(r("docs.rs/wry?search=x#top"), "https://docs.rs/wry?search=x#top");
        assert_eq!(r("example.com:8443/a"), "https://example.com:8443/a");
        assert_eq!(r("xn--bcher-kva.example"), "https://xn--bcher-kva.example");
    }

    #[test]
    fn explicit_schemes_pass_through() {
        assert_eq!(r("http://example.com"), "http://example.com");
        assert_eq!(r("HTTPS://Example.com/Path"), "HTTPS://Example.com/Path");
        assert_eq!(r("about:blank"), "about:blank");
        assert_eq!(r("file:///C:/x.html"), "file:///C:/x.html");
        assert_eq!(r("view-source:https://example.com"), "view-source:https://example.com");
    }

    #[test]
    fn local_hosts_get_http() {
        assert_eq!(r("localhost"), "http://localhost");
        assert_eq!(r("localhost:3000/app"), "http://localhost:3000/app");
        assert_eq!(r("127.0.0.1:8080"), "http://127.0.0.1:8080");
        assert_eq!(r("192.168.1.1"), "http://192.168.1.1");
        assert_eq!(r("[::1]:8080"), "http://[::1]:8080");
    }

    #[test]
    fn windows_paths_become_file_urls() {
        assert_eq!(r(r"C:\Users\me\page.html"), "file:///C:/Users/me/page.html");
        assert_eq!(r(r"d:\my docs\a#b.html"), "file:///d:/my%20docs/a%23b.html");
        assert_eq!(r(r"\\server\share\my file.html"), "file://server/share/my%20file.html");
    }

    #[test]
    fn everything_else_is_a_search() {
        assert_eq!(r("rust"), format!("{SEARCH_URL}rust"));
        assert_eq!(r("hello world"), format!("{SEARCH_URL}hello+world"));
        assert_eq!(r("c++ templates"), format!("{SEARCH_URL}c%2B%2B+templates"));
        assert_eq!(r("what is 1.5"), format!("{SEARCH_URL}what+is+1.5"));
        assert_eq!(r("1.5"), format!("{SEARCH_URL}1.5"));
        assert_eq!(r("999.1.1.1"), format!("{SEARCH_URL}999.1.1.1"));
        assert_eq!(r("example.com:abc"), format!("{SEARCH_URL}example.com%3Aabc"));
    }

    #[test]
    fn dangerous_schemes_are_searched_not_run() {
        assert!(r("javascript:alert(1)").starts_with(SEARCH_URL));
        assert!(r("vbscript:msgbox").starts_with(SEARCH_URL));
        assert!(r("data:text/html,<script>").starts_with(SEARCH_URL));
    }

    #[test]
    fn query_encoding_handles_unicode() {
        assert_eq!(encode_query("café & tea"), "caf%C3%A9+%26+tea");
    }
}
