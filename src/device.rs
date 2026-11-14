//! What kind of PC the browser runs on, for efficiency mode.

use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

/// Installed memory (RAM) in bytes, as Windows reports it.
pub fn total_memory() -> Option<u64> {
    let mut status = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    // SAFETY: `status` is a correctly sized, writable MEMORYSTATUSEX with dwLength set.
    let ok = unsafe { GlobalMemoryStatusEx(&mut status) };
    (ok != 0 && status.ullTotalPhys > 0).then_some(status.ullTotalPhys)
}

/// Processor threads available to the browser.
pub fn processor_count() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
}

/// A low-end PC: about 4 GB of memory or less (Windows reports a little
/// under the installed amount), or at most two processor threads.
pub fn is_low_end(memory: Option<u64>, processors: usize) -> bool {
    const LOW_MEMORY: u64 = 4608 * 1024 * 1024; // 4.5 GiB
    memory.is_some_and(|m| m <= LOW_MEMORY) || processors <= 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_pcs() {
        const GB: u64 = 1024 * 1024 * 1024;
        assert!(is_low_end(Some(4 * GB - 200 * 1024 * 1024), 8), "4 GB PC (reported a bit lower)");
        assert!(is_low_end(Some(2 * GB), 4));
        assert!(is_low_end(Some(16 * GB), 2), "dual-core");
        assert!(!is_low_end(Some(8 * GB), 4));
        assert!(!is_low_end(None, 8), "unknown memory isn't treated as low");
    }

    #[test]
    fn reads_this_pc() {
        assert!(total_memory().is_some_and(|m| m > 256 * 1024 * 1024));
        assert!(processor_count() >= 1);
    }
}
