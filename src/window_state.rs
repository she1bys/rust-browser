//! Where the window was and how big it was, saved in
//! `%LOCALAPPDATA%\RustBrowser\window.json` so the browser opens the same way
//! next time.

use std::{fs, io, path::Path};

use serde::{Deserialize, Serialize};

use crate::settings::write_atomic;

/// The file in the profile folder.
pub const FILE_NAME: &str = "window.json";

/// Smallest window that is restored; matches the window's minimum size
/// (420 x 300 logical pixels) at 100% scaling.
const MIN_WIDTH: u32 = 420;
const MIN_HEIGHT: u32 = 300;
/// Anything bigger is a broken file, not a real window.
const MAX_SIDE: u32 = 32_768;
/// How much of the window's top strip (tabs and the space to drag the window
/// by) must be on one monitor for the saved place to be used. Less than that,
/// e.g. after a second monitor was unplugged, and the window opens at the
/// default place instead, so it can never be lost off screen.
const MIN_VISIBLE_WIDTH: i64 = 160;
const MIN_VISIBLE_HEIGHT: i64 = 30;
/// Height of the strip at the top that has to be reachable.
const TOP_STRIP: i64 = 38;

/// The window's normal (not maximized, not minimized) place, in physical
/// pixels: the outer top-left corner and the inner size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowState {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub maximized: bool,
}

/// A monitor's area in physical pixels: x, y, width, height.
pub type MonitorRect = (i32, i32, u32, u32);

impl WindowState {
    /// The saved state, if there is a usable one.
    pub fn load(path: &Path) -> Option<Self> {
        let state: WindowState = serde_json::from_str(&fs::read_to_string(path).ok()?).ok()?;
        state.is_sane().then_some(state)
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        write_atomic(path, &serde_json::to_string(self)?)
    }

    fn is_sane(&self) -> bool {
        (MIN_WIDTH..=MAX_SIDE).contains(&self.width) && (MIN_HEIGHT..=MAX_SIDE).contains(&self.height)
    }

    /// Whether enough of the top of the window would be on one of `monitors`
    /// to see it and drag it.
    pub fn is_visible_on(&self, monitors: &[MonitorRect]) -> bool {
        let (left, top) = (i64::from(self.x), i64::from(self.y));
        let right = left + i64::from(self.width);
        let bottom = top + TOP_STRIP.min(i64::from(self.height));
        monitors.iter().any(|&(mx, my, mw, mh)| {
            let (m_left, m_top) = (i64::from(mx), i64::from(my));
            let (m_right, m_bottom) = (m_left + i64::from(mw), m_top + i64::from(mh));
            let overlap_w = right.min(m_right) - left.max(m_left);
            let overlap_h = bottom.min(m_bottom) - top.max(m_top);
            overlap_w >= MIN_VISIBLE_WIDTH && overlap_h >= MIN_VISIBLE_HEIGHT
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL_HD: MonitorRect = (0, 0, 1920, 1080);

    fn state(x: i32, y: i32, width: u32, height: u32) -> WindowState {
        WindowState { x, y, width, height, maximized: false }
    }

    #[test]
    fn round_trips_through_a_file() {
        let path = std::env::temp_dir().join(format!("rust-browser-window-{}.json", std::process::id()));
        let saved = WindowState { x: -1500, y: 40, width: 1400, height: 900, maximized: true };
        saved.save(&path).unwrap();
        assert_eq!(WindowState::load(&path), Some(saved));

        fs::write(&path, "nonsense").unwrap();
        assert_eq!(WindowState::load(&path), None);
        let _ = fs::remove_file(&path);
        assert_eq!(WindowState::load(&path), None);
    }

    #[test]
    fn rejects_impossible_sizes() {
        let path = std::env::temp_dir().join(format!("rust-browser-window-bad-{}.json", std::process::id()));
        for text in [
            r#"{"x":0,"y":0,"width":10,"height":800}"#,
            r#"{"x":0,"y":0,"width":1280,"height":0}"#,
            r#"{"x":0,"y":0,"width":99999,"height":800}"#,
            r#"{"x":0,"y":0,"width":-5,"height":800}"#,
        ] {
            fs::write(&path, text).unwrap();
            assert_eq!(WindowState::load(&path), None, "{text}");
        }
        // Older files without `maximized` still load.
        fs::write(&path, r#"{"x":10,"y":20,"width":1280,"height":800}"#).unwrap();
        assert_eq!(WindowState::load(&path), Some(state(10, 20, 1280, 800)));
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn checks_the_window_is_on_a_monitor() {
        assert!(state(100, 100, 1280, 800).is_visible_on(&[FULL_HD]));
        // Mostly off the right edge, but the left part of the tab strip shows.
        assert!(state(1700, 100, 1280, 800).is_visible_on(&[FULL_HD]));
        // Only a sliver shows: not enough to grab.
        assert!(!state(1850, 100, 1280, 800).is_visible_on(&[FULL_HD]));
        // The tab strip is above the top of the screen.
        assert!(!state(100, -500, 1280, 800).is_visible_on(&[FULL_HD]));
        // On a second monitor to the left that has since been unplugged.
        let left_monitor = (-1920, 0, 1920, 1080);
        assert!(state(-1500, 50, 1280, 800).is_visible_on(&[FULL_HD, left_monitor]));
        assert!(!state(-1500, 50, 1280, 800).is_visible_on(&[FULL_HD]));
        // No monitors known: never trust the saved place.
        assert!(!state(100, 100, 1280, 800).is_visible_on(&[]));
    }
}
