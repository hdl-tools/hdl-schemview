//! Process memory probes for the scalability scenarios (#22, #155).
//!
//! Latency alone cannot decide either issue: #22's premise is a design *too
//! large to materialize*, and #155's payoff is skipping the owned copy. Both are
//! memory questions, so every scenario reports peak RSS alongside wall time.
//!
//! Deliberately **dependency-free** — the benchmark must build on an offline
//! machine from the existing lockfile, so this hand-declares the two platform
//! probes instead of pulling in a crate:
//!
//! * Windows — `K32GetProcessMemoryInfo` (kernel32) `PeakWorkingSetSize`, and
//!   `GlobalMemoryStatusEx` for machine RAM (#240).
//! * Linux — `/proc/self/status` `VmHWM` (peak) / `VmRSS` (current), and
//!   `/proc/meminfo` for machine RAM.
//! * Anything else — `None`, reported as `n/a` rather than a fabricated zero.

/// Peak resident set of this process in bytes, or `None` where unsupported.
///
/// Monotonic within a process, so a scenario may read it after dropping the
/// design and still see the high-water mark.
pub fn peak_rss_bytes() -> Option<u64> {
    imp::peak_rss_bytes()
}

/// Current resident set of this process in bytes, or `None` where unsupported.
pub fn current_rss_bytes() -> Option<u64> {
    imp::current_rss_bytes()
}

/// Bytes as MB (10^6), for report tables.
pub fn mb(bytes: u64) -> f64 {
    bytes as f64 / 1e6
}

/// Machine RAM as `(total, available)` bytes, or `None` where unsupported (#240).
///
/// Not a *process* probe like the two above, but it belongs here for the same
/// reason they do: the collector's environment table needs it, and reading it
/// costs one more hand-declared entry point instead of a dependency. "Available"
/// is the OS's own estimate of what a new allocation could claim — the number
/// that decides whether the 1M basis will survive — not merely free pages.
pub fn ram_bytes() -> Option<(u64, u64)> {
    imp::ram_bytes()
}

#[cfg(windows)]
mod imp {
    use core::ffi::c_void;

    /// `PROCESS_MEMORY_COUNTERS` (psapi.h). Every field is written by the OS; we
    /// read two, so the rest are dead by construction.
    #[allow(dead_code)]
    #[repr(C)]
    struct ProcessMemoryCounters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }

    impl ProcessMemoryCounters {
        fn new() -> Self {
            Self {
                cb: std::mem::size_of::<Self>() as u32,
                page_fault_count: 0,
                peak_working_set_size: 0,
                working_set_size: 0,
                quota_peak_paged_pool_usage: 0,
                quota_paged_pool_usage: 0,
                quota_peak_non_paged_pool_usage: 0,
                quota_non_paged_pool_usage: 0,
                pagefile_usage: 0,
                peak_pagefile_usage: 0,
            }
        }
    }

    /// `MEMORYSTATUSEX` (sysinfoapi.h). Same deal — the OS fills every field.
    #[allow(dead_code)]
    #[repr(C)]
    struct MemoryStatusEx {
        length: u32,
        memory_load: u32,
        total_phys: u64,
        avail_phys: u64,
        total_page_file: u64,
        avail_page_file: u64,
        total_virtual: u64,
        avail_virtual: u64,
        avail_extended_virtual: u64,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn K32GetProcessMemoryInfo(
            process: *mut c_void,
            counters: *mut ProcessMemoryCounters,
            cb: u32,
        ) -> i32;
        fn GlobalMemoryStatusEx(buffer: *mut MemoryStatusEx) -> i32;
    }

    fn counters() -> Option<ProcessMemoryCounters> {
        let mut c = ProcessMemoryCounters::new();
        let cb = c.cb;
        // SAFETY: `c` is a correctly sized and aligned PROCESS_MEMORY_COUNTERS
        // with `cb` set as the API requires, and `GetCurrentProcess` returns a
        // pseudo-handle that is always valid for the calling process.
        let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, cb) };
        (ok != 0).then_some(c)
    }

    pub fn peak_rss_bytes() -> Option<u64> {
        counters().map(|c| c.peak_working_set_size as u64)
    }

    pub fn current_rss_bytes() -> Option<u64> {
        counters().map(|c| c.working_set_size as u64)
    }

    pub fn ram_bytes() -> Option<(u64, u64)> {
        let mut s = MemoryStatusEx {
            length: std::mem::size_of::<MemoryStatusEx>() as u32,
            memory_load: 0,
            total_phys: 0,
            avail_phys: 0,
            total_page_file: 0,
            avail_page_file: 0,
            total_virtual: 0,
            avail_virtual: 0,
            avail_extended_virtual: 0,
        };
        // SAFETY: `s` is a correctly sized and aligned MEMORYSTATUSEX with
        // `length` set as the API requires; the call only writes into it.
        let ok = unsafe { GlobalMemoryStatusEx(&mut s) };
        (ok != 0).then_some((s.total_phys, s.avail_phys))
    }
}

#[cfg(target_os = "linux")]
mod imp {
    /// Read a `VmXxx:  N kB` line out of `/proc/self/status`.
    fn status_kb(key: &str) -> Option<u64> {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        for line in status.lines() {
            let Some(rest) = line.strip_prefix(key) else {
                continue;
            };
            let Some(rest) = rest.strip_prefix(':') else {
                continue;
            };
            let kb = rest.split_whitespace().next()?;
            return kb.parse::<u64>().ok().map(|v| v * 1024);
        }
        None
    }

    pub fn peak_rss_bytes() -> Option<u64> {
        status_kb("VmHWM")
    }

    pub fn current_rss_bytes() -> Option<u64> {
        status_kb("VmRSS")
    }

    /// Read a `MemXxx:  N kB` line out of `/proc/meminfo`.
    fn meminfo_kb(key: &str) -> Option<u64> {
        let info = std::fs::read_to_string("/proc/meminfo").ok()?;
        for line in info.lines() {
            let Some(rest) = line.strip_prefix(key) else {
                continue;
            };
            let Some(rest) = rest.strip_prefix(':') else {
                continue;
            };
            let kb = rest.split_whitespace().next()?;
            return kb.parse::<u64>().ok().map(|v| v * 1024);
        }
        None
    }

    pub fn ram_bytes() -> Option<(u64, u64)> {
        // `MemAvailable` is absent from the `/proc` emulations some environments
        // ship (git-bash), so fall back to `MemFree` rather than losing the total
        // as well.
        let total = meminfo_kb("MemTotal")?;
        let avail = meminfo_kb("MemAvailable")
            .or_else(|| meminfo_kb("MemFree"))
            .unwrap_or(0);
        Some((total, avail))
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod imp {
    pub fn peak_rss_bytes() -> Option<u64> {
        None
    }

    pub fn current_rss_bytes() -> Option<u64> {
        None
    }

    pub fn ram_bytes() -> Option<(u64, u64)> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The probe must either work or say it doesn't — never report a bogus zero.
    #[test]
    fn peak_rss_is_plausible_or_absent() {
        match peak_rss_bytes() {
            // A Rust process that just started still holds more than 1 MB.
            Some(bytes) => assert!(bytes > 1_000_000, "implausible peak RSS: {bytes} bytes"),
            None => {
                #[cfg(any(windows, target_os = "linux"))]
                panic!("peak RSS probe returned None on a supported platform");
            }
        }
    }

    #[test]
    fn current_never_exceeds_peak() {
        if let (Some(cur), Some(peak)) = (current_rss_bytes(), peak_rss_bytes()) {
            assert!(cur <= peak, "current RSS {cur} exceeds peak {peak}");
        }
    }
}
