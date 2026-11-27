//! Liquid Glass for the browser's own pages (see `ui/glass.js`): its script
//! and styles are put into each page at the `<!--liquid-glass-->` marker.

const CSS: &str = include_str!("../ui/glass.css");
const JS: &str = include_str!("../ui/glass.js");
const MARKER: &str = "<!--liquid-glass-->";

/// `html` with Liquid Glass built in. `enabled` switches it on from the first
/// frame (the page can switch it on and off later through the `lg` class).
pub fn page(html: &str, enabled: bool) -> String {
    let on = if enabled { "<script>document.documentElement.classList.add('lg')</script>" } else { "" };
    html.replacen(MARKER, &format!("<style>\n{CSS}</style>\n<script>\n{JS}</script>{on}"), 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_browser_page_has_the_marker() {
        for (name, html) in [
            ("index.html", include_str!("../ui/index.html")),
            ("menu.html", include_str!("../ui/menu.html")),
            ("newtab.html", include_str!("../ui/newtab.html")),
        ] {
            assert_eq!(html.matches(MARKER).count(), 1, "{name}");
            let built = page(html, true);
            assert!(!built.contains(MARKER), "{name}");
            assert!(built.contains("window.liquidGlass") && built.contains("--lg-wallpaper"), "{name}");
            assert!(built.contains("classList.add('lg')"), "{name}");
            assert!(!page(html, false).contains("classList.add('lg')"), "{name}");
        }
    }

    #[test]
    fn script_and_styles_cannot_end_their_tags_early() {
        assert!(!CSS.to_ascii_lowercase().contains("</style"));
        assert!(!JS.to_ascii_lowercase().contains("</script"));
    }
}
