//! How much memory a process uses, as Windows Task Manager shows it in its
//! "Memory" column (the private working set: memory used by this process
//! alone, held in RAM right now).

use crate::ipc::RendererInfo;

use windows_sys::Win32::{
    Foundation::CloseHandle,
    System::{
        ProcessStatus::{
            GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX, PROCESS_MEMORY_COUNTERS_EX2,
        },
        Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
    },
};

/// Memory used by process `pid`, in bytes, or `None` if it can't be read
/// (for example because the process has already ended).
pub fn process_memory(pid: u32) -> Option<u64> {
    if pid == 0 {
        return None;
    }
    // SAFETY: the handle is checked before use and always closed; the
    // counters structs are plain data with `cb` set to their real size.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return None;
        }

        // Windows 10 1809+: the private working set, exactly Task Manager's figure.
        let mut ex2: PROCESS_MEMORY_COUNTERS_EX2 = std::mem::zeroed();
        ex2.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX2>() as u32;
        let mut result = if GetProcessMemoryInfo(process, (&raw mut ex2).cast::<PROCESS_MEMORY_COUNTERS>(), ex2.cb) != 0
        {
            Some(ex2.PrivateWorkingSetSize as u64)
        } else {
            None
        };

        // Older Windows: private bytes, a close stand-in.
        if result.is_none() {
            let mut ex: PROCESS_MEMORY_COUNTERS_EX = std::mem::zeroed();
            ex.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
            if GetProcessMemoryInfo(process, (&raw mut ex).cast::<PROCESS_MEMORY_COUNTERS>(), ex.cb) != 0 {
                result = Some(ex.PrivateUsage as u64);
            }
        }

        CloseHandle(process);
        result
    }
}

/// The engine processes that draw page `page` (a tab's main frame id),
/// including those for frames it embeds from other sites, and how many of
/// the `other_pages` (the other tabs) share at least one of them.
pub fn processes_for_page(renderers: &[RendererInfo], page: u32, other_pages: &[u32]) -> (Vec<u32>, usize) {
    let mine: Vec<&RendererInfo> = renderers.iter().filter(|r| r.pages.contains(&page)).collect();
    let pids = mine.iter().map(|r| r.pid).collect();
    let shared =
        other_pages.iter().filter(|&&other| other != page && mine.iter().any(|r| r.pages.contains(&other))).count();
    (pids, shared)
}

/// The processes that would end if the pages in `sleeping` were unloaded:
/// those that draw only pages from that set. A process shared with any other
/// page (another tab, or the browser's own UI) stays, so isn't counted.
pub fn processes_freed_by(renderers: &[RendererInfo], sleeping: &[u32]) -> Vec<u32> {
    renderers
        .iter()
        .filter(|r| !r.pages.is_empty() && r.pages.iter().all(|p| sleeping.contains(p)))
        .map(|r| r.pid)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_processes_used_solely_by_sleeping_pages_are_freed() {
        let renderers = [
            renderer(100, &[1]),    // tab 1 alone
            renderer(200, &[1, 2]), // shared by tabs 1 and 2
            renderer(300, &[3]),    // tab 3 alone
            renderer(400, &[]),     // a spare process with no pages: not ours to count
        ];
        assert_eq!(processes_freed_by(&renderers, &[1]), vec![100]);
        assert_eq!(processes_freed_by(&renderers, &[1, 2]), vec![100, 200]);
        assert_eq!(processes_freed_by(&renderers, &[3]), vec![300]);
        assert_eq!(processes_freed_by(&renderers, &[]), Vec::<u32>::new());
    }

    fn renderer(pid: u32, pages: &[u32]) -> RendererInfo {
        RendererInfo { pid, pages: pages.to_vec() }
    }

    #[test]
    fn finds_a_pages_processes_and_who_shares_them() {
        let renderers = [
            renderer(100, &[1]),    // tab 1's own process
            renderer(200, &[1, 2]), // an embedded site used by tabs 1 and 2
            renderer(300, &[3]),    // tab 3 alone
        ];
        assert_eq!(processes_for_page(&renderers, 1, &[2, 3]), (vec![100, 200], 1));
        assert_eq!(processes_for_page(&renderers, 3, &[1, 2]), (vec![300], 0));
        // A page the engine doesn't know (e.g. still starting) has no processes.
        assert_eq!(processes_for_page(&renderers, 9, &[1, 2, 3]), (vec![], 0));
        // The page itself in the "others" list isn't counted as sharing.
        assert_eq!(processes_for_page(&renderers, 3, &[3]), (vec![300], 0));
    }

    #[test]
    fn reads_this_processs_memory() {
        let bytes = process_memory(std::process::id()).expect("can read our own memory use");
        // A running test binary uses at least some memory, and far less than 64 GB.
        assert!(bytes > 100 * 1024 && bytes < 64 * 1024 * 1024 * 1024, "{bytes}");
    }

    #[test]
    fn missing_processes_read_as_none() {
        assert_eq!(process_memory(0), None);
        // Process ids are multiples of 4 on Windows, so this one can never exist.
        assert_eq!(process_memory(4_294_967_293), None);
    }
}
