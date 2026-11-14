//! Single-instance coordination on Windows via a named pipe.
//!
//! If another instance is already running, this process sends its optional
//! command-line argument over the pipe and tells `main` to exit. The running
//! instance receives the message and wakes its event loop to focus or open a tab.

use std::{
    ffi::OsString,
    fs::OpenOptions,
    io::{BufRead, BufReader, Write},
    os::windows::ffi::OsStrExt,
    time::{Duration, Instant},
};

use tao::event_loop::EventLoopProxy;
use windows_sys::Win32::{
    Foundation::{
        GetLastError, ERROR_ACCESS_DENIED, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, HANDLE, INVALID_HANDLE_VALUE,
    },
    Storage::FileSystem::PIPE_ACCESS_INBOUND,
    System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, WaitNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
        PIPE_WAIT,
    },
};

use crate::ipc::UserEvent;

pub enum InstanceRole {
    /// This is the primary browser process; keep running.
    Primary,
    /// Handed off to an already-running browser process; exit now.
    Forwarded,
}

/// Pipe names are shared by everyone on the PC, so the name includes the
/// Windows user name: two people signed in at once each get their own browser.
fn pipe_name() -> String {
    let user: String = std::env::var("USERNAME")
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect();
    match crate::profile_override() {
        // A separate profile (tests) is a separate instance.
        Some(dir) => {
            let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
            for byte in dir.to_string_lossy().to_lowercase().bytes() {
                hash = (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
            }
            format!(r"\\.\pipe\RustBrowser-SingleInstance-v2-{user}-{hash:016x}")
        }
        None => format!(r"\\.\pipe\RustBrowser-SingleInstance-v2-{user}"),
    }
}

/// Attempts to forward `arg` to an existing instance, or becomes the primary
/// instance listening for future launches.
///
/// `take_over` is used when restarting after an engine crash: the old process
/// is about to exit, so instead of handing over to it, wait (up to a few
/// seconds) for it to go and then become the primary instance.
pub fn acquire(arg: Option<&str>, proxy: EventLoopProxy<UserEvent>, take_over: bool) -> InstanceRole {
    let name = pipe_name();
    if !take_over && try_forward(&name, arg) {
        return InstanceRole::Forwarded;
    }

    let wide_name: Vec<u16> = OsString::from(&name).encode_wide().chain(std::iter::once(0)).collect();
    let deadline = Instant::now() + Duration::from_secs(5);
    let pipe = loop {
        // SAFETY: valid NUL-terminated UTF-16 pipe name; standard Win32 pipe creation.
        let handle = unsafe {
            CreateNamedPipeW(
                wide_name.as_ptr(),
                PIPE_ACCESS_INBOUND,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                1,
                0,
                4096,
                0,
                std::ptr::null(),
            )
        };
        if handle != INVALID_HANDLE_VALUE {
            break handle;
        }
        // SAFETY: thread-local Win32 error getter.
        let in_use = unsafe { GetLastError() } == ERROR_ACCESS_DENIED;
        if in_use && take_over && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        // Another process raced us to create the pipe: hand over to it.
        if in_use && !take_over && try_forward(&name, arg) {
            return InstanceRole::Forwarded;
        }
        // Couldn't set up the pipe; run anyway, just without single-instance.
        return InstanceRole::Primary;
    };

    let pipe_handle = SendHandle(pipe);
    std::thread::spawn(move || listen(pipe_handle, proxy));
    InstanceRole::Primary
}

/// Serves the pipe forever: one line per later launch.
fn listen(pipe: SendHandle, proxy: EventLoopProxy<UserEvent>) {
    let handle = pipe.0;
    loop {
        // SAFETY: `handle` is a valid named pipe handle owned exclusively by this thread.
        let connected = unsafe { ConnectNamedPipe(handle, std::ptr::null_mut()) } != 0
            || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED;
        if connected {
            let mut reader = BufReader::new(PipeReader(handle));
            let mut line = String::new();
            if reader.read_line(&mut line).is_ok() {
                let trimmed = line.trim_end_matches(['\r', '\n']);
                let url = if trimmed.is_empty() { None } else { Some(trimmed.to_string()) };
                let _ = proxy.send_event(UserEvent::ExternalOpen(url));
            }
        } else {
            // Typically a launch that connected and left before we started
            // listening. Without the pause below, an error that keeps
            // repeating would spin this thread at 100% CPU.
            std::thread::sleep(Duration::from_millis(50));
        }
        // Reset the pipe for the next launch, whatever happened above.
        // SAFETY: `handle` is valid; disconnecting an unconnected pipe is harmless.
        unsafe {
            DisconnectNamedPipe(handle);
        }
    }
}

fn try_forward(name: &str, arg: Option<&str>) -> bool {
    let payload = format!("{}\n", arg.unwrap_or(""));
    for _ in 0..3 {
        match OpenOptions::new().write(true).open(name) {
            Ok(mut pipe) => {
                if pipe.write_all(payload.as_bytes()).is_ok() {
                    let _ = pipe.flush();
                    return true;
                }
                return false;
            }
            Err(_) => {
                // SAFETY: thread-local Win32 error getter.
                if unsafe { GetLastError() } == ERROR_PIPE_BUSY {
                    let wide: Vec<u16> = OsString::from(name).encode_wide().chain(std::iter::once(0)).collect();
                    // SAFETY: valid NUL-terminated UTF-16 pipe name.
                    unsafe { WaitNamedPipeW(wide.as_ptr(), 250) };
                } else {
                    return false;
                }
            }
        }
    }
    false
}

struct SendHandle(HANDLE);
// SAFETY: Named pipe handle is created on the main thread and then transferred
// exclusively to the single listener thread.
unsafe impl Send for SendHandle {}

struct PipeReader(HANDLE);

impl std::io::Read for PipeReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut read = 0u32;
        // SAFETY: `self.0` is a live pipe handle and `buf` is valid for `buf.len()` bytes.
        let ok = unsafe {
            windows_sys::Win32::Storage::FileSystem::ReadFile(
                self.0,
                buf.as_mut_ptr(),
                buf.len().min(u32::MAX as usize) as u32,
                &mut read,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            let err = std::io::Error::last_os_error();
            // ERROR_BROKEN_PIPE (109) signals clean EOF on named pipes.
            if err.raw_os_error() == Some(109) {
                return Ok(0);
            }
            return Err(err);
        }
        Ok(read as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_name_is_per_user_and_safe() {
        let name = pipe_name();
        assert!(name.starts_with(r"\\.\pipe\RustBrowser-SingleInstance-v2-"));
        let suffix = &name[r"\\.\pipe\".len()..];
        assert!(!suffix.contains('\\') && !suffix.contains('/'), "{name}");
    }
}
