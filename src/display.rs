//! Display information (the monitor's refresh rate, shown in Settings >
//! About) and the command-line options passed to the WebView2 engine.
//!
//! Chromium already draws at the monitor's own refresh rate (60, 144, 165,
//! 240 Hz ...) by itself, so no special tuning is needed for smooth scrolling
//! and animation on high-refresh-rate screens. Options that force it (an
//! unlimited frame rate, a 1 ms system timer, rendering while the window is
//! covered, overriding the GPU driver blocklist) were measured to bring no
//! smoothness gain here while costing processor time, battery and heat, so
//! they are deliberately not used.

use std::mem::{size_of, zeroed};

use windows_sys::Win32::{
    Foundation::HWND,
    Graphics::Gdi::{
        EnumDisplaySettingsW, GetMonitorInfoW, MonitorFromWindow, DEVMODEW, ENUM_CURRENT_SETTINGS, MONITORINFOEXW,
        MONITOR_DEFAULTTONEAREST,
    },
};

/// Options for the WebView2 engine.
///
/// `msWebOOUI` and `msPdfOOUI` are turned off as `wry` does by default (Edge
/// pop-up UI pieces that don't belong in a separate browser). Unlike `wry`'s
/// default, Microsoft Defender SmartScreen (`msSmartScreenProtection`) is
/// left on: it is the engine's protection against phishing and malware
/// sites, and the "Protect me from dangerous sites" setting controls it per tab.
pub const BROWSER_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI --disable-logging";

/// Returns the current refresh rate (in Hz) of the monitor hosting `hwnd`,
/// falling back to the primary display if the window handle has no monitor yet.
pub fn monitor_refresh_hz(hwnd: isize) -> Option<u32> {
    if hwnd != 0 {
        if let Some(hz) = refresh_hz_for_hwnd(hwnd as HWND) {
            return Some(hz);
        }
    }
    refresh_hz_for_device(std::ptr::null())
}

fn refresh_hz_for_hwnd(hwnd: HWND) -> Option<u32> {
    // SAFETY: `MonitorFromWindow` accepts any HWND and returns a valid HMONITOR
    // or null; `MONITORINFOEXW` has `cbSize` initialized before calling
    // `GetMonitorInfoW`.
    unsafe {
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        if monitor.is_null() {
            return None;
        }
        let mut info: MONITORINFOEXW = zeroed();
        info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(monitor, (&raw mut info.monitorInfo).cast()) == 0 {
            return None;
        }
        refresh_hz_for_device(info.szDevice.as_ptr())
    }
}

fn refresh_hz_for_device(device_name: *const u16) -> Option<u32> {
    // SAFETY: `DEVMODEW` has `dmSize` initialized before calling
    // `EnumDisplaySettingsW`; `device_name` is either null (primary display) or
    // points to the null-terminated `MONITORINFOEXW.szDevice` buffer.
    unsafe {
        let mut mode: DEVMODEW = zeroed();
        mode.dmSize = size_of::<DEVMODEW>() as u16;
        if EnumDisplaySettingsW(device_name, ENUM_CURRENT_SETTINGS, &raw mut mode) == 0 {
            return None;
        }
        normalize_refresh_hz(mode.dmDisplayFrequency)
    }
}

/// Windows reports `0` or `1` when a display driver uses "hardware default"
/// rather than an explicit integer frequency.
fn normalize_refresh_hz(raw_hz: u32) -> Option<u32> {
    (raw_hz > 1).then_some(raw_hz)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_args_keep_smartscreen_and_power_saving_on() {
        assert!(BROWSER_ARGS.contains("msWebOOUI") && BROWSER_ARGS.contains("msPdfOOUI"));
        // Safety and battery features of the engine must stay enabled.
        for forbidden in [
            "msSmartScreenProtection",
            "--disable-frame-rate-limit",
            "CalculateNativeWinOcclusion",
            "UseEcoQoSForBackgroundProcess",
            "--enable-gpu-rasterization",
            "--enable-zero-copy",
        ] {
            assert!(!BROWSER_ARGS.contains(forbidden), "{forbidden} must not be passed to the engine");
        }
    }

    #[test]
    fn normalize_refresh_hz_filters_sentinel_values() {
        assert_eq!(normalize_refresh_hz(0), None);
        assert_eq!(normalize_refresh_hz(1), None);
        assert_eq!(normalize_refresh_hz(60), Some(60));
        assert_eq!(normalize_refresh_hz(165), Some(165));
        assert_eq!(normalize_refresh_hz(240), Some(240));
    }

    #[test]
    fn querying_primary_display_does_not_panic() {
        if let Some(hz) = monitor_refresh_hz(0) {
            assert!(hz >= 24);
        }
    }
}
