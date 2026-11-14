//! The built-in ad and tracker blocker.
//!
//! Requests to known advertising and tracking domains get an empty "blocked"
//! answer instead of being downloaded, so the ads, their scripts and their
//! tracking pixels never load. That makes pages lighter: less to download,
//! less script to run, less memory.
//!
//! WebView2 does the first, coarse match itself (one request filter per
//! domain, see [`request_filters`]), so ordinary requests never reach Rust;
//! [`blocked_entry`] then checks the exact host. A site is never blocked on
//! its own pages (visiting taboola.com still works).

use std::{collections::HashSet, sync::OnceLock};

/// Advertising, ad-measurement and cross-site tracking domains. Each entry
/// also covers its subdomains. Kept to services whose blocking doesn't break
/// ordinary sites (no sign-in, payment, video player or content-delivery
/// domains).
const BLOCKED_DOMAINS: &[&str] = &[
    // Google ad serving
    "doubleclick.net",
    "googlesyndication.com",
    "googleadservices.com",
    "googletagservices.com",
    "adservice.google.com",
    "google-analytics.com",
    // Ad exchanges and networks
    "amazon-adsystem.com",
    "adnxs.com",
    "adsrvr.org",
    "rubiconproject.com",
    "pubmatic.com",
    "openx.net",
    "casalemedia.com",
    "indexww.com",
    "criteo.com",
    "criteo.net",
    "bidswitch.net",
    "contextweb.com",
    "sovrn.com",
    "lijit.com",
    "smartadserver.com",
    "adform.net",
    "yieldmo.com",
    "triplelift.com",
    "3lift.com",
    "sharethrough.com",
    "teads.tv",
    "gumgum.com",
    "33across.com",
    "media.net",
    "spotxchange.com",
    "spotx.tv",
    "springserve.com",
    "smaato.net",
    "kargo.com",
    "undertone.com",
    "conversantmedia.com",
    "dotomi.com",
    "mathtag.com",
    "advertising.com",
    "adtechus.com",
    "serving-sys.com",
    "flashtalking.com",
    "zedo.com",
    "adroll.com",
    "bidr.io",
    "bttrack.com",
    "turn.com",
    "yieldlab.net",
    "adition.com",
    "improvedigital.com",
    "e-planning.net",
    "adkernel.com",
    "adtelligent.com",
    "onetag-sys.com",
    "rhythmone.com",
    "unrulymedia.com",
    "emxdgt.com",
    "ads.yahoo.com",
    "ads.linkedin.com",
    "ads-twitter.com",
    "ads.pinterest.com",
    "ct.pinterest.com",
    "bat.bing.com",
    // Sponsored-content widgets ("around the web")
    "taboola.com",
    "outbrain.com",
    "outbrainimg.com",
    "revcontent.com",
    "mgid.com",
    "zergnet.com",
    "lockerdome.com",
    "ntv.io",
    "adskeeper.com",
    "adskeeper.co.uk",
    // Pop-up, pop-under and redirect ad networks
    "popads.net",
    "popcash.net",
    "propellerads.com",
    "exoclick.com",
    "exosrv.com",
    "juicyads.com",
    "trafficjunky.net",
    "adsterra.com",
    "hilltopads.net",
    "onclickads.net",
    "adcash.com",
    "clickadu.com",
    "a-ads.com",
    "ad-maven.com",
    // Ad verification and measurement
    "moatads.com",
    "doubleverify.com",
    "adsafeprotected.com",
    "scorecardresearch.com",
    "imrworldwide.com",
    "quantserve.com",
    "quantcount.com",
    // Data brokers and cross-site identity syncing
    "demdex.net",
    "everesttech.net",
    "bluekai.com",
    "exelator.com",
    "agkn.com",
    "tapad.com",
    "crwdcntrl.net",
    "krxd.net",
    "rlcdn.com",
    "id5-sync.com",
    "liadm.com",
    "eyeota.net",
    "ml314.com",
    "adsymptotic.com",
    // Session recording and behavioural tracking
    "hotjar.com",
    "mouseflow.com",
    "fullstory.com",
    "crazyegg.com",
    "clarity.ms",
    "analytics.tiktok.com",
    "analytics.twitter.com",
    "analytics.yahoo.com",
];

fn blocked_set() -> &'static HashSet<&'static str> {
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| BLOCKED_DOMAINS.iter().copied().collect())
}

/// WebView2 request filters (URL wildcards) that catch every blocked domain
/// and its subdomains. They may also catch a few other URLs (a `*` can span
/// the path), which [`blocked_entry`] then lets through.
pub fn request_filters() -> impl Iterator<Item = String> {
    BLOCKED_DOMAINS.iter().flat_map(|d| [format!("*://{d}/*"), format!("*://*.{d}/*")])
}

/// The host of an `http(s)` URL, lower-cased, without port or credentials.
pub fn host_of(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host_port = authority.rsplit('@').next()?;
    let host = if host_port.starts_with('[') {
        host_port.split(']').next().map(|h| &h[1..])?
    } else {
        host_port.split(':').next()?
    };
    let host = host.trim_end_matches('.');
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// The blocklist entry covering `host` (the host itself or one of its parent
/// domains), if any.
pub fn blocked_entry(host: &str) -> Option<&'static str> {
    let set = blocked_set();
    let mut candidate = host;
    loop {
        if let Some(entry) = set.get(candidate) {
            return Some(entry);
        }
        candidate = candidate.split_once('.')?.1;
    }
}

/// True if `host` is `domain` or one of its subdomains.
fn is_within(host: &str, domain: &str) -> bool {
    host == domain || host.strip_suffix(domain).is_some_and(|prefix| prefix.ends_with('.'))
}

/// Should a request for `request_url`, made by a page at `page_url`, be blocked?
pub fn should_block(request_url: &str, page_url: &str) -> bool {
    let Some(host) = host_of(request_url) else { return false };
    let Some(entry) = blocked_entry(&host) else { return false };
    // Never block a site on its own pages.
    !host_of(page_url).is_some_and(|page| is_within(&page, entry))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_hosts() {
        assert_eq!(host_of("https://Ads.Example.com:8443/x?y#z").as_deref(), Some("ads.example.com"));
        assert_eq!(host_of("http://user:pw@tracker.net/p").as_deref(), Some("tracker.net"));
        assert_eq!(host_of("https://example.com.").as_deref(), Some("example.com"));
        assert_eq!(host_of("https://[::1]:80/").as_deref(), Some("::1"));
        assert_eq!(host_of("file:///C:/x.html"), None);
        assert_eq!(host_of("https:///nohost"), None);
    }

    #[test]
    fn blocks_listed_domains_and_their_subdomains() {
        let page = "https://news.example.com/story";
        for url in [
            "https://securepubads.g.doubleclick.net/tag/js/gpt.js",
            "https://pagead2.googlesyndication.com/pagead/js/adsbygoogle.js",
            "https://cdn.taboola.com/libtrc/x/loader.js",
            "https://www.google-analytics.com/analytics.js",
            "http://ads.yahoo.com/pixel",
            "https://px.ads.linkedin.com/collect",
        ] {
            assert!(should_block(url, page), "{url}");
        }
    }

    #[test]
    fn leaves_everything_else_alone() {
        let page = "https://news.example.com/story";
        for url in [
            "https://www.google.com/search?q=doubleclick.net",
            "https://example.com/ads.doubleclick.net/image.png", // in the path only
            "https://notdoubleclick.net/x",                      // not a subdomain
            "https://www.youtube.com/watch?v=1",
            "https://yahoo.com/",
            "https://www.linkedin.com/in/someone",
            "https://twitter.com/home",
            "data:text/html,hi",
        ] {
            assert!(!should_block(url, page), "{url}");
        }
    }

    #[test]
    fn never_blocks_a_site_on_its_own_pages() {
        assert!(!should_block("https://www.taboola.com/logo.png", "https://www.taboola.com/about"));
        assert!(!should_block("https://cdn.taboola.com/x.js", "https://taboola.com/"));
        // ...but does on other sites.
        assert!(should_block("https://cdn.taboola.com/x.js", "https://example.com/"));
    }

    #[test]
    fn filters_cover_every_domain_and_entries_are_clean() {
        let filters: Vec<String> = request_filters().collect();
        assert_eq!(filters.len(), BLOCKED_DOMAINS.len() * 2);
        assert!(filters.contains(&"*://*.doubleclick.net/*".to_string()));
        assert!(filters.contains(&"*://doubleclick.net/*".to_string()));
        let unique: HashSet<_> = BLOCKED_DOMAINS.iter().collect();
        assert_eq!(unique.len(), BLOCKED_DOMAINS.len(), "no duplicates");
        for d in BLOCKED_DOMAINS {
            assert!(
                d.contains('.')
                    && !d.starts_with('.')
                    && d.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-'),
                "{d}"
            );
        }
    }
}
