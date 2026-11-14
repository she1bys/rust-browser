//! Native WebView2 hooks and actions for each tab beyond what `wry` exposes.

use std::{cell::RefCell, collections::VecDeque, rc::Rc};

use tao::event_loop::EventLoopProxy;
use webview2_com::{
    take_pwstr, ContainsFullScreenElementChangedEventHandler, FaviconChangedEventHandler, FindStartCompletedHandler,
    FocusChangedEventHandler, GetProcessExtendedInfosCompletedHandler, HistoryChangedEventHandler,
    IsDocumentPlayingAudioChangedEventHandler, IsMutedChangedEventHandler,
    Microsoft::Web::WebView2::Win32::{
        ICoreWebView2, ICoreWebView2Environment13, ICoreWebView2Environment15, ICoreWebView2Environment8,
        ICoreWebView2FrameInfo, ICoreWebView2FrameInfo2, ICoreWebView2ProcessExtendedInfoCollection,
        ICoreWebView2Profile3, ICoreWebView2Settings4, ICoreWebView2Settings8, ICoreWebView2_12, ICoreWebView2_13,
        ICoreWebView2_15, ICoreWebView2_16, ICoreWebView2_20, ICoreWebView2_25, ICoreWebView2_28, ICoreWebView2_3,
        ICoreWebView2_8, COREWEBVIEW2_PRINT_DIALOG_KIND_BROWSER, COREWEBVIEW2_PROCESS_FAILED_KIND,
        COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED,
        COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED,
        COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_UNRESPONSIVE, COREWEBVIEW2_PROCESS_KIND,
        COREWEBVIEW2_PROCESS_KIND_RENDERER, COREWEBVIEW2_TRACKING_PREVENTION_LEVEL_BALANCED,
        COREWEBVIEW2_TRACKING_PREVENTION_LEVEL_BASIC, COREWEBVIEW2_TRACKING_PREVENTION_LEVEL_NONE,
        COREWEBVIEW2_TRACKING_PREVENTION_LEVEL_STRICT,
    },
    NewWindowRequestedEventHandler, ProcessFailedEventHandler, ShowSaveAsUICompletedHandler,
    StatusBarTextChangedEventHandler, TrySuspendCompletedHandler, ZoomFactorChangedEventHandler,
};
use windows::core::{Interface, BOOL, HSTRING, PWSTR};
use wry::{WebView, WebViewExtWindows};

use crate::ipc::{ProcessFailure, RendererInfo, TabId, UserEvent};
use crate::settings::TrackingPrevention;

/// Whether each pending new-window request came from a click, oldest first.
/// Filled here, emptied by the browser's new-window handler (see
/// `Browser::ensure_tab_webview`), which runs for the same requests in the
/// same order slightly later.
pub type PopupGestures = Rc<RefCell<VecDeque<bool>>>;

/// Attaches navigation history, favicon, status text, fullscreen, zoom,
/// audio, crash and pop-up listeners to `webview` for `tab`.
pub fn attach(
    webview: &WebView,
    tab: TabId,
    proxy: EventLoopProxy<UserEvent>,
    gestures: PopupGestures,
) -> windows::core::Result<()> {
    let core = webview.webview();
    let controller = webview.controller();

    let history_proxy = proxy.clone();
    let mut token = 0i64;
    // SAFETY: live COM objects owned by the UI thread; out-pointers are valid locals.
    unsafe {
        core.add_HistoryChanged(
            &HistoryChangedEventHandler::create(Box::new(move |sender, _| {
                let Some(sender) = sender else { return Ok(()) };
                let (mut back, mut forward) = (BOOL(0), BOOL(0));
                sender.CanGoBack(&mut back)?;
                sender.CanGoForward(&mut forward)?;
                let _ = history_proxy.send_event(UserEvent::HistoryChanged {
                    id: tab,
                    can_go_back: back.as_bool(),
                    can_go_forward: forward.as_bool(),
                });
                Ok(())
            })),
            &mut token,
        )?;

        let fs_proxy = proxy.clone();
        core.add_ContainsFullScreenElementChanged(
            &ContainsFullScreenElementChangedEventHandler::create(Box::new(move |sender, _| {
                let Some(sender) = sender else { return Ok(()) };
                let mut fullscreen = BOOL(0);
                sender.ContainsFullScreenElement(&mut fullscreen)?;
                let _ = fs_proxy.send_event(UserEvent::FullScreenChanged { id: tab, fullscreen: fullscreen.as_bool() });
                Ok(())
            })),
            &mut token,
        )?;

        // Record, for the pop-up blocker, whether each new-window request
        // came from a click. Runs in the same event as wry's own handler.
        core.add_NewWindowRequested(
            &NewWindowRequestedEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else { return Ok(()) };
                let mut user = BOOL(1);
                args.IsUserInitiated(&mut user)?;
                gestures.borrow_mut().push_back(user.as_bool());
                Ok(())
            })),
            &mut token,
        )?;

        let crash_proxy = proxy.clone();
        core.add_ProcessFailed(
            &ProcessFailedEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else { return Ok(()) };
                let mut kind = COREWEBVIEW2_PROCESS_FAILED_KIND::default();
                args.ProcessFailedKind(&mut kind)?;
                let failure = match kind {
                    COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED => ProcessFailure::PageCrashed,
                    COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_UNRESPONSIVE => ProcessFailure::PageUnresponsive,
                    COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED => ProcessFailure::EngineExited,
                    _ => ProcessFailure::Other,
                };
                let _ = crash_proxy.send_event(UserEvent::ProcessFailed { id: tab, failure });
                Ok(())
            })),
            &mut token,
        )?;

        if let Ok(core8) = core.cast::<ICoreWebView2_8>() {
            let send_audio = {
                let proxy = proxy.clone();
                move |sender: Option<ICoreWebView2>| -> windows::core::Result<()> {
                    let Some(core8) = sender.and_then(|s| s.cast::<ICoreWebView2_8>().ok()) else { return Ok(()) };
                    let (mut playing, mut muted) = (BOOL(0), BOOL(0));
                    core8.IsDocumentPlayingAudio(&mut playing)?;
                    core8.IsMuted(&mut muted)?;
                    let _ = proxy.send_event(UserEvent::AudioChanged {
                        id: tab,
                        playing: playing.as_bool(),
                        muted: muted.as_bool(),
                    });
                    Ok(())
                }
            };
            let send_muted = send_audio.clone();
            let _ = core8.add_IsDocumentPlayingAudioChanged(
                &IsDocumentPlayingAudioChangedEventHandler::create(Box::new(move |sender, _| send_audio(sender))),
                &mut token,
            );
            let _ = core8.add_IsMutedChanged(
                &IsMutedChangedEventHandler::create(Box::new(move |sender, _| send_muted(sender))),
                &mut token,
            );
        }

        if let Ok(core12) = core.cast::<ICoreWebView2_12>() {
            let status_proxy = proxy.clone();
            let _ = core12.add_StatusBarTextChanged(
                &StatusBarTextChangedEventHandler::create(Box::new(move |sender, _| {
                    let Some(sender) = sender else { return Ok(()) };
                    if let Ok(sender12) = sender.cast::<ICoreWebView2_12>() {
                        let mut text = PWSTR::null();
                        sender12.StatusBarText(&mut text)?;
                        let _ =
                            status_proxy.send_event(UserEvent::StatusTextChanged { id: tab, text: take_pwstr(text) });
                    }
                    Ok(())
                })),
                &mut token,
            );
        }

        let zoom_proxy = proxy.clone();
        controller.add_ZoomFactorChanged(
            &ZoomFactorChangedEventHandler::create(Box::new(move |sender, _| {
                let Some(sender) = sender else { return Ok(()) };
                let mut factor = 1.0f64;
                sender.ZoomFactor(&mut factor)?;
                let _ = zoom_proxy.send_event(UserEvent::ZoomChanged { id: tab, factor });
                Ok(())
            })),
            &mut token,
        )?;

        let focus_proxy = proxy.clone();
        let _ = controller.add_GotFocus(
            &FocusChangedEventHandler::create(Box::new(move |_, _| {
                let _ = focus_proxy.send_event(UserEvent::TabFocused(tab));
                Ok(())
            })),
            &mut token,
        );

        if let Ok(core15) = core.cast::<ICoreWebView2_15>() {
            let fav_proxy = proxy;
            let _ = core15.add_FaviconChanged(
                &FaviconChangedEventHandler::create(Box::new(move |sender, _| {
                    let Some(sender) = sender else { return Ok(()) };
                    if let Ok(sender15) = sender.cast::<ICoreWebView2_15>() {
                        let mut text = PWSTR::null();
                        sender15.FaviconUri(&mut text)?;
                        let _ = fav_proxy.send_event(UserEvent::FaviconChanged { id: tab, url: take_pwstr(text) });
                    }
                    Ok(())
                })),
                &mut token,
            );
        }
    }
    Ok(())
}

/// Watches the browser UI's own web view for engine failures.
pub fn attach_ui_crash(webview: &WebView, proxy: EventLoopProxy<UserEvent>) -> windows::core::Result<()> {
    let mut token = 0i64;
    // SAFETY: live COM object owned by the UI thread; `token` is a valid out-pointer.
    unsafe {
        webview.webview().add_ProcessFailed(
            &ProcessFailedEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else { return Ok(()) };
                let mut kind = COREWEBVIEW2_PROCESS_FAILED_KIND::default();
                args.ProcessFailedKind(&mut kind)?;
                let failure = match kind {
                    COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED => ProcessFailure::PageCrashed,
                    COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_UNRESPONSIVE => ProcessFailure::PageUnresponsive,
                    COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED => ProcessFailure::EngineExited,
                    _ => ProcessFailure::Other,
                };
                let _ = proxy.send_event(UserEvent::UiProcessFailed(failure));
                Ok(())
            })),
            &mut token,
        )
    }
}

/// Freezes a hidden tab: its scripts and timers stop and the engine drops
/// some caches. WebView2 resumes it by itself as soon as it is shown again.
pub fn try_suspend(webview: &WebView) -> windows::core::Result<()> {
    // SAFETY: live COM object owned by the UI thread.
    unsafe {
        webview
            .webview()
            .cast::<ICoreWebView2_3>()?
            .TrySuspend(&TrySuspendCompletedHandler::create(Box::new(|_, _| Ok(()))))
    }
}

/// The id of the tab's main frame, which the engine uses to say which
/// process draws which page.
pub fn main_frame_id(webview: &WebView) -> Option<u32> {
    let mut id = 0u32;
    // SAFETY: live COM object owned by the UI thread; `id` is a valid out-pointer.
    unsafe { webview.webview().cast::<ICoreWebView2_20>().ok()?.FrameId(&mut id).ok()? };
    Some(id)
}

/// Asks WebView2 which page processes exist and which pages (main frames)
/// each one draws, including frames embedded from other sites. The answer
/// arrives later as a `RendererInfos` event.
pub fn query_renderers(webview: &WebView, proxy: EventLoopProxy<UserEvent>) -> windows::core::Result<()> {
    let environment = webview.environment().cast::<ICoreWebView2Environment13>()?;
    let handler = GetProcessExtendedInfosCompletedHandler::create(Box::new(move |result, list| {
        let mut renderers = Vec::new();
        if let (Ok(()), Some(list)) = (result, list) {
            // SAFETY: COM objects handed to us by WebView2 on the UI thread.
            unsafe { collect_renderers(&list, &mut renderers) };
        }
        let _ = proxy.send_event(UserEvent::RendererInfos(renderers));
        Ok(())
    }));
    // SAFETY: live COM object owned by the UI thread.
    unsafe { environment.GetProcessExtendedInfos(&handler) }
}

/// Every process of the WebView2 engine that serves this browser (the main
/// engine process, graphics, network and helper processes, and one per page
/// or site), as `(process id, is a page process)`.
pub fn engine_processes(webview: &WebView) -> Vec<(u32, bool)> {
    let mut out = Vec::new();
    // SAFETY: live COM objects owned by the UI thread; out-pointers are valid locals.
    unsafe {
        let Ok(environment) = webview.environment().cast::<ICoreWebView2Environment8>() else { return out };
        let Ok(list) = environment.GetProcessInfos() else { return out };
        let mut count = 0u32;
        if list.Count(&mut count).is_err() {
            return out;
        }
        for index in 0..count {
            let Ok(info) = list.GetValueAtIndex(index) else { continue };
            let (mut pid, mut kind) = (0i32, COREWEBVIEW2_PROCESS_KIND::default());
            if info.ProcessId(&mut pid).is_ok() && pid > 0 {
                let _ = info.Kind(&mut kind);
                out.push((pid as u32, kind == COREWEBVIEW2_PROCESS_KIND_RENDERER));
            }
        }
    }
    out
}

/// # Safety
/// `list` must be a live collection from `GetProcessExtendedInfos`.
unsafe fn collect_renderers(list: &ICoreWebView2ProcessExtendedInfoCollection, out: &mut Vec<RendererInfo>) {
    let mut count = 0u32;
    if unsafe { list.Count(&mut count) }.is_err() {
        return;
    }
    for index in 0..count {
        let Ok(info) = (unsafe { list.GetValueAtIndex(index) }) else { continue };
        let Ok(process) = (unsafe { info.ProcessInfo() }) else { continue };
        let mut kind = COREWEBVIEW2_PROCESS_KIND::default();
        let mut pid = 0i32;
        if unsafe { process.Kind(&mut kind) }.is_err() || kind != COREWEBVIEW2_PROCESS_KIND_RENDERER {
            continue;
        }
        if unsafe { process.ProcessId(&mut pid) }.is_err() || pid <= 0 {
            continue;
        }
        let mut pages = Vec::new();
        if let Ok(frames) = unsafe { info.AssociatedFrameInfos() } {
            if let Ok(iterator) = unsafe { frames.GetIterator() } {
                let mut has = BOOL(0);
                while unsafe { iterator.HasCurrent(&mut has) }.is_ok() && has.as_bool() {
                    if let Ok(frame) = unsafe { iterator.GetCurrent() } {
                        if let Some(root) = unsafe { root_frame_id(&frame) } {
                            if !pages.contains(&root) {
                                pages.push(root);
                            }
                        }
                    }
                    if unsafe { iterator.MoveNext(&mut has) }.is_err() {
                        break;
                    }
                }
            }
        }
        out.push(RendererInfo { pid: pid as u32, pages });
    }
}

/// The id of the page (main frame) a frame belongs to: follows the chain of
/// parent frames up to the top.
///
/// # Safety
/// `frame` must be a live frame info object from WebView2.
unsafe fn root_frame_id(frame: &ICoreWebView2FrameInfo) -> Option<u32> {
    let mut current = frame.cast::<ICoreWebView2FrameInfo2>().ok()?;
    // Frames nest only a few levels deep; the limit just guards against a loop.
    for _ in 0..32 {
        match unsafe { current.ParentFrameInfo() }.and_then(|parent| parent.cast::<ICoreWebView2FrameInfo2>()) {
            Ok(parent) => current = parent,
            Err(_) => break,
        }
    }
    let mut id = 0u32;
    unsafe { current.FrameId(&mut id) }.ok()?;
    Some(id)
}

pub fn go_back(webview: &WebView) -> windows::core::Result<()> {
    // SAFETY: live ICoreWebView2 owned by the UI thread.
    unsafe { webview.webview().GoBack() }
}

pub fn go_forward(webview: &WebView) -> windows::core::Result<()> {
    // SAFETY: live ICoreWebView2 owned by the UI thread.
    unsafe { webview.webview().GoForward() }
}

pub fn stop(webview: &WebView) -> windows::core::Result<()> {
    // SAFETY: live ICoreWebView2 owned by the UI thread.
    unsafe { webview.webview().Stop() }
}

/// Opens WebView2's find bar. If the engine reports afterwards that it
/// couldn't, a `FindFailed` event is sent so the browser can fall back.
pub fn find(webview: &WebView, tab: TabId, proxy: EventLoopProxy<UserEvent>) -> windows::core::Result<()> {
    // SAFETY: live COM objects owned by the UI thread.
    unsafe {
        let finder = webview.webview().cast::<ICoreWebView2_28>()?.Find()?;
        let options = webview.environment().cast::<ICoreWebView2Environment15>()?.CreateFindOptions()?;
        options.SetFindTerm(&HSTRING::new())?;
        options.SetSuppressDefaultFindDialog(false)?;
        finder.Start(
            &options,
            &FindStartCompletedHandler::create(Box::new(move |result| {
                if result.is_err() {
                    let _ = proxy.send_event(UserEvent::FindFailed(tab));
                }
                Ok(())
            })),
        )
    }
}

/// Fallback for opening the find bar: presses Ctrl+F for the user. The page
/// must already have keyboard focus.
pub fn press_ctrl_f() {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VK_CONTROL,
    };
    let key = |vk: u16, up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: if up { KEYEVENTF_KEYUP } else { 0 },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let inputs = [key(VK_CONTROL, false), key(b'F'.into(), false), key(b'F'.into(), true), key(VK_CONTROL, true)];
    // SAFETY: a valid array of fully initialised INPUT structures.
    unsafe {
        SendInput(inputs.len() as u32, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32);
    }
}

/// Opens the print preview.
pub fn print(webview: &WebView) -> windows::core::Result<()> {
    // SAFETY: live COM object owned by the UI thread.
    unsafe { webview.webview().cast::<ICoreWebView2_16>()?.ShowPrintUI(COREWEBVIEW2_PRINT_DIALOG_KIND_BROWSER) }
}

/// Opens the "Save page as" dialog.
pub fn save_page(webview: &WebView) -> windows::core::Result<()> {
    // SAFETY: live COM object owned by the UI thread.
    unsafe {
        webview
            .webview()
            .cast::<ICoreWebView2_25>()?
            .ShowSaveAsUI(&ShowSaveAsUICompletedHandler::create(Box::new(|_, _| Ok(()))))
    }
}

pub fn open_devtools(webview: &WebView) -> windows::core::Result<()> {
    // SAFETY: live COM object owned by the UI thread.
    unsafe { webview.webview().OpenDevToolsWindow() }
}

pub fn set_muted(webview: &WebView, muted: bool) -> windows::core::Result<()> {
    // SAFETY: live COM object owned by the UI thread.
    unsafe { webview.webview().cast::<ICoreWebView2_8>()?.SetIsMuted(muted) }
}

/// Disables host objects and web messaging on untrusted page tabs so WebView2
/// does not inject `window.chrome.webview` bindings into every document/iframe.
pub fn configure_page_webview(webview: &WebView) -> windows::core::Result<()> {
    // SAFETY: live COM object owned by the UI thread.
    unsafe {
        let settings = webview.webview().Settings()?;
        settings.SetAreHostObjectsAllowed(false)?;
        settings.SetIsWebMessageEnabled(false)
    }
}

/// Strips unnecessary conveniences (zoom, pinch-zoom, swipe navigation,
/// autofill, host objects) from internal chrome/menu UI WebViews while keeping IPC.
pub fn configure_ui_webview(webview: &WebView) -> windows::core::Result<()> {
    use webview2_com::Microsoft::Web::WebView2::Win32::{ICoreWebView2Settings5, ICoreWebView2Settings6};
    // SAFETY: live COM objects owned by the UI thread.
    unsafe {
        let settings = webview.webview().Settings()?;
        settings.SetAreHostObjectsAllowed(false)?;
        settings.SetIsZoomControlEnabled(false)?;
        if let Ok(s4) = settings.cast::<ICoreWebView2Settings4>() {
            let _ = s4.SetIsGeneralAutofillEnabled(false);
            let _ = s4.SetIsPasswordAutosaveEnabled(false);
        }
        if let Ok(s5) = settings.cast::<ICoreWebView2Settings5>() {
            let _ = s5.SetIsPinchZoomEnabled(false);
        }
        if let Ok(s6) = settings.cast::<ICoreWebView2Settings6>() {
            let _ = s6.SetIsSwipeNavigationEnabled(false);
        }
    }
    Ok(())
}

/// Applies the privacy settings to `webview`: password saving and autofill
/// (per web view) and tracking prevention (for its profile; private tabs
/// have a profile of their own).
pub fn apply_privacy(
    webview: &WebView,
    save_passwords: bool,
    autofill: bool,
    tracking: TrackingPrevention,
    safe_browsing: bool,
) -> windows::core::Result<()> {
    // SAFETY: live COM objects owned by the UI thread.
    unsafe {
        // Microsoft Defender SmartScreen checks for this web view. Older
        // WebView2 runtimes lack the switch; they keep their default.
        if let Ok(settings8) = webview.webview().Settings()?.cast::<ICoreWebView2Settings8>() {
            settings8.SetIsReputationCheckingRequired(safe_browsing)?;
        }
    }
    let level = match tracking {
        TrackingPrevention::Off => COREWEBVIEW2_TRACKING_PREVENTION_LEVEL_NONE,
        TrackingPrevention::Basic => COREWEBVIEW2_TRACKING_PREVENTION_LEVEL_BASIC,
        TrackingPrevention::Balanced => COREWEBVIEW2_TRACKING_PREVENTION_LEVEL_BALANCED,
        TrackingPrevention::Strict => COREWEBVIEW2_TRACKING_PREVENTION_LEVEL_STRICT,
    };
    // SAFETY: live COM objects owned by the UI thread.
    unsafe {
        let core = webview.webview();
        let settings = core.Settings()?.cast::<ICoreWebView2Settings4>()?;
        settings.SetIsPasswordAutosaveEnabled(save_passwords)?;
        settings.SetIsGeneralAutofillEnabled(autofill)?;
        let profile = core.cast::<ICoreWebView2_13>()?.Profile()?.cast::<ICoreWebView2Profile3>()?;
        profile.SetPreferredTrackingPreventionLevel(level)
    }
}
