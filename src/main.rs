// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bookmarks;
mod browser;
mod display;
mod download_hook;
mod downloads;
mod history;
mod ipc;
mod memory;
mod menu;
mod newtab;
mod session;
mod settings;
mod shortcut_hook;
mod single_instance;
mod updater;
mod url_input;
mod webview_hooks;

use std::path::PathBuf;

use tao::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder},
    platform::windows::{IconExtWindows, WindowBuilderExtWindows},
    window::WindowBuilder,
};

use browser::Browser;
use ipc::UserEvent;
use single_instance::InstanceRole;

fn main() {
    if let Err(e) = run() {
        fatal(&format!(
            "Rust Browser could not start.\n\n{e}\n\nMake sure the Microsoft Edge WebView2 Runtime is installed."
        ));
        std::process::exit(1);
    }
}

/// Passed when the browser restarts itself after the WebView2 engine crashed.
pub(crate) const RESTORE_ARG: &str = "--restore-session";

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let first_arg = std::env::args().nth(1);
    let restoring = first_arg.as_deref() == Some(RESTORE_ARG);
    let cli_arg = if restoring { None } else { first_arg };
    if matches!(
        single_instance::acquire(cli_arg.as_deref(), event_loop.create_proxy(), restoring),
        InstanceRole::Forwarded
    ) {
        return Ok(());
    }

    // No Windows title bar: the tab strip is the title bar, with its own
    // minimize / maximize / close buttons (see ui/index.html). The window
    // keeps its native shadow, rounded corners and side/bottom resize borders.
    // The app icon built into the .exe (resource id 1, see build.rs).
    let icon = tao::window::Icon::from_resource(1, None).ok();
    let window = WindowBuilder::new()
        .with_decorations(false)
        .with_undecorated_shadow(true)
        .with_window_icon(icon.clone())
        .with_taskbar_icon(icon)
        .with_title("Rust Browser")
        .with_inner_size(LogicalSize::new(1280.0, 800.0))
        .with_min_inner_size(LogicalSize::new(420.0, 300.0))
        .build(&event_loop)?;

    let mut browser = Browser::new(&event_loop, window, event_loop.create_proxy(), data_dir(), restoring)?;

    // `rust-browser.exe <url or search>` opens that instead of the home page.
    browser.open_initial_tabs(cli_arg.as_deref())?;
    browser.start_memory_saver();
    updater::clean_up_old_download();

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::WindowEvent { window_id, event, .. } => {
                if browser.is_menu_window(window_id) {
                    if matches!(event, WindowEvent::CloseRequested)
                        || (matches!(event, WindowEvent::Focused(false)) && !browser.menu_owns_focus())
                    {
                        browser.hide_menu();
                    }
                    return;
                }
                match event {
                    WindowEvent::Focused(true) => {
                        if !browser.menu_owns_focus() {
                            browser.hide_menu();
                        }
                    }
                    WindowEvent::CloseRequested => {
                        browser.request_close();
                        if browser.exit_requested() {
                            *control_flow = ControlFlow::Exit;
                        }
                    }
                    WindowEvent::Moved(_) => {
                        browser.hide_menu();
                        browser.update_display_rate();
                    }
                    WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                        browser.hide_menu();
                        browser.layout();
                        browser.update_display_rate();
                    }
                    _ => {}
                }
            }
            Event::UserEvent(event) => {
                browser.handle(event);
                if browser.exit_requested() {
                    *control_flow = ControlFlow::Exit;
                }
            }
            Event::MainEventsCleared => {
                browser.flush_sync();
            }
            _ => {}
        }
    })
}

/// Cookies, cache, settings and the downloads list live in `%LOCALAPPDATA%\RustBrowser`.
/// Falls back to WebView2's default (next to the .exe) if that isn't available.
fn data_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("RustBrowser");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

#[cfg(windows)]
pub(crate) fn fatal(message: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let (text, caption) = (wide(message), wide("Rust Browser"));
    // SAFETY: both pointers are valid, NUL-terminated UTF-16 strings that outlive the call.
    unsafe {
        MessageBoxW(std::ptr::null_mut(), text.as_ptr(), caption.as_ptr(), MB_OK | MB_ICONERROR);
    }
}

#[cfg(not(windows))]
pub(crate) fn fatal(message: &str) {
    eprintln!("{message}");
}
