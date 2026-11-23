//! Bookmark files: the HTML format every browser can export and import
//! (the "Netscape bookmark file"). Firefox, Chrome, Edge, Safari and Opera all
//! read and write it, so this is how bookmarks move to and from them.

use crate::bookmarks::Bookmark;
use crate::session::is_restorable;

/// More than this many bookmarks in one file is not a real bookmarks file.
const MAX_BOOKMARKS: usize = 20_000;

/// The bookmarks as a bookmark file. They go in the bookmarks bar folder,
/// since Rust Browser's bookmarks bar shows all of them.
pub fn to_html(bookmarks: &[Bookmark]) -> String {
    let mut html = String::from(
        "<!DOCTYPE NETSCAPE-Bookmark-file-1>\n\
         <!-- This is an automatically generated file.\n     \
         It will be read and overwritten.\n     \
         DO NOT EDIT! -->\n\
         <META HTTP-EQUIV=\"Content-Type\" CONTENT=\"text/html; charset=UTF-8\">\n\
         <TITLE>Bookmarks</TITLE>\n\
         <H1>Bookmarks</H1>\n\
         <DL><p>\n    \
         <DT><H3 PERSONAL_TOOLBAR_FOLDER=\"true\">Bookmarks bar</H3>\n    \
         <DL><p>\n",
    );
    for bookmark in bookmarks {
        html.push_str("        <DT><A HREF=\"");
        html.push_str(&escape(&bookmark.url));
        html.push_str("\">");
        html.push_str(&escape(&bookmark.title));
        html.push_str("</A>\n");
    }
    html.push_str("    </DL><p>\n</DL><p>\n");
    html
}

/// Every bookmark in a bookmark file, in order, once each. Folders are
/// flattened. Addresses that can't be opened safely (`javascript:`
/// bookmarklets, Firefox's `place:` queries and the like) are left out.
pub fn parse_html(text: &str) -> Vec<Bookmark> {
    // ASCII lowercasing keeps every byte where it was, so positions found
    // in `lower` are positions in `text` too.
    let lower = text.to_ascii_lowercase();
    let mut bookmarks: Vec<Bookmark> = Vec::new();
    let mut pos = 0;
    while let Some(found) = lower[pos..].find("<a") {
        let start = pos + found;
        pos = start + 2;
        // `<a` must be the whole tag name (not `<abbr>` or `<address>`).
        if !lower[pos..].starts_with(|c: char| c.is_ascii_whitespace()) {
            continue;
        }
        let Some((href, tag_end)) = href_attribute(text, pos) else { continue };
        let body_end = lower[tag_end..].find("</a").map_or(text.len(), |i| tag_end + i);
        pos = body_end;
        let url = decode_entities(href.trim());
        if !is_restorable(&url) || bookmarks.iter().any(|b| b.url == url) {
            continue;
        }
        let title = collapse_whitespace(&decode_entities(&strip_tags(&text[tag_end..body_end])));
        let title = if title.is_empty() { url.clone() } else { title };
        bookmarks.push(Bookmark { url, title });
        if bookmarks.len() == MAX_BOOKMARKS {
            break;
        }
    }
    bookmarks
}

/// Reads the attributes of the tag that starts just before `from`. Returns
/// the HREF value and where the tag ends (just after its `>`).
fn href_attribute(text: &str, from: usize) -> Option<(&str, usize)> {
    let bytes = text.as_bytes();
    let mut i = from;
    let mut href = None;
    loop {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        match bytes.get(i)? {
            b'>' => return href.map(|h| (h, i + 1)),
            b'/' => {
                i += 1;
                continue;
            }
            _ => {}
        }
        let name_start = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() && !matches!(bytes[i], b'=' | b'>' | b'/') {
            i += 1;
        }
        let name = &text[name_start..i];
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if bytes.get(i) != Some(&b'=') {
            continue; // an attribute without a value
        }
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let value = match bytes.get(i)? {
            &quote @ (b'"' | b'\'') => {
                let value_start = i + 1;
                let len = text[value_start..].find(quote as char)?;
                i = value_start + len + 1;
                &text[value_start..value_start + len]
            }
            _ => {
                let value_start = i;
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>' {
                    i += 1;
                }
                &text[value_start..i]
            }
        };
        if href.is_none() && name.eq_ignore_ascii_case("href") {
            href = Some(value);
        }
    }
}

/// Text with any tags inside it removed (`<b>Rust</b>` -> `Rust`).
fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Turns `&amp;`, `&quot;`, `&#39;`, `&#x2F;` and friends back into characters.
fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let decoded = rest.find(';').filter(|&end| end <= 10).and_then(|end| {
            let name = &rest[1..end];
            let c = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                _ => {
                    let number = name.strip_prefix('#')?;
                    let code = match number.strip_prefix(['x', 'X']) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                        None => number.parse().ok()?,
                    };
                    char::from_u32(code)
                }
            }?;
            Some((c, end + 1))
        });
        match decoded {
            Some((c, len)) => {
                out.push(c);
                rest = &rest[len..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Makes text safe to put in HTML, inside an attribute or between tags.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bookmark(url: &str, title: &str) -> Bookmark {
        Bookmark { url: url.into(), title: title.into() }
    }

    #[test]
    fn exported_files_read_back_the_same() {
        let saved = vec![
            bookmark("https://www.rust-lang.org/", "Rust"),
            bookmark("https://example.com/search?q=a&b=\"c\"", "Tom & Jerry <\"quotes\"> 'and' more"),
            bookmark("https://例え.jp/パス", "日本語のタイトル"),
            bookmark("file:///C:/Users/me/notes.html", "Notes"),
        ];
        let html = to_html(&saved);
        assert!(html.starts_with("<!DOCTYPE NETSCAPE-Bookmark-file-1>"));
        assert!(!html.contains("Tom & Jerry"), "text is escaped");
        assert_eq!(parse_html(&html), saved);
        assert_eq!(parse_html(&to_html(&[])), Vec::new());
    }

    #[test]
    fn reads_files_from_other_browsers() {
        // Trimmed from a real Firefox export: folders, icons, dates,
        // separators and smart folders.
        let firefox = r#"<!DOCTYPE NETSCAPE-Bookmark-file-1>
<META HTTP-EQUIV="Content-Type" CONTENT="text/html; charset=UTF-8">
<TITLE>Bookmarks</TITLE>
<H1>Bookmarks Menu</H1>
<DL><p>
    <DT><H3 ADD_DATE="1700000000" LAST_MODIFIED="1700000001">Mozilla Firefox</H3>
    <DL><p>
        <DT><A HREF="https://support.mozilla.org/products/firefox" ADD_DATE="1700000000" ICON="data:image/png;base64,iVBORw0KGgo=">Get Help</A>
        <DT><A HREF="place:sort=8&amp;maxResults=10">Recent Tags</A>
    </DL><p>
    <HR>
    <DT><A HREF="javascript:alert(document.title)">A bookmarklet</A>
    <DT><a href='https://developer.mozilla.org/en-US/docs/Web?a=1&amp;b=2' tags="dev">MDN
        Web   Docs</a>
    <DT><A HREF=https://unquoted.example/path>Unquoted</A>
    <DT><A HREF="https://www.rust-lang.org/">Rust &#8211; a language &#x2764;</A>
    <DT><A HREF="https://www.rust-lang.org/">Duplicate of Rust</A>
    <DT><A HREF="https://empty-title.example/"></A>
    <DT><ABBR>Not a link</ABBR> <ADDRESS>Nor this</ADDRESS>
</DL><p>
"#;
        assert_eq!(
            parse_html(firefox),
            vec![
                bookmark("https://support.mozilla.org/products/firefox", "Get Help"),
                bookmark("https://developer.mozilla.org/en-US/docs/Web?a=1&b=2", "MDN Web Docs"),
                bookmark("https://unquoted.example/path", "Unquoted"),
                bookmark("https://www.rust-lang.org/", "Rust – a language ❤"),
                bookmark("https://empty-title.example/", "https://empty-title.example/"),
            ]
        );
    }

    #[test]
    fn copes_with_broken_files() {
        for text in ["", "not html at all", "<a", "<a href", "<a href=\"https://x.example/", "<a href=>x</a>", "&#xZZ;"]
        {
            assert_eq!(parse_html(text), Vec::new(), "{text:?}");
        }
        // No closing tag: the title runs to the end of the file.
        assert_eq!(parse_html("<A HREF=\"https://a.example/\">Title"), vec![bookmark("https://a.example/", "Title")]);
        assert_eq!(decode_entities("a &bogus; b &amp c &#9999999999; d"), "a &bogus; b &amp c &#9999999999; d");
        assert_eq!(decode_entities("&lt;b&gt; &amp;amp;"), "<b> &amp;");
    }
}
