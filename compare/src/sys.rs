//! Process-level measurements: CPU time, context switches, resident memory,
//! and a description of the host.

use std::time::Duration;

/// Total user + system CPU time of this process.
pub fn process_cpu_time() -> Option<Duration> {
    imp::process_cpu_time()
}

/// Total context switches of all threads in this process, where the OS
/// exposes them (Linux). `None` elsewhere.
pub fn context_switches() -> Option<u64> {
    imp::context_switches()
}

/// Resident set size in bytes.
pub fn resident_bytes() -> Option<u64> {
    imp::resident_bytes()
}

/// One line describing the CPU, OS, and compiler.
pub fn host_description() -> String {
    format!(
        "{} | {} {} | {}",
        imp::cpu_name().unwrap_or_else(|| "unknown CPU".into()),
        std::env::consts::OS,
        imp::os_version().unwrap_or_default(),
        env!("COMPARE_RUSTC_VERSION"),
    )
}

#[cfg(target_os = "linux")]
mod imp {
    use std::time::Duration;

    pub fn process_cpu_time() -> Option<Duration> {
        // /proc/self/stat fields 14 and 15: utime and stime in clock ticks.
        let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
        let after = &stat[stat.rfind(')')? + 2..];
        let f: Vec<&str> = after.split_whitespace().collect();
        let ticks: u64 = f.get(11)?.parse::<u64>().ok()? + f.get(12)?.parse::<u64>().ok()?;
        // Clock ticks are 100 per second on every Linux the reference hosts run.
        Some(Duration::from_millis(ticks * 10))
    }

    pub fn context_switches() -> Option<u64> {
        let mut total = 0u64;
        for task in std::fs::read_dir("/proc/self/task").ok()? {
            let status = std::fs::read_to_string(task.ok()?.path().join("status")).ok()?;
            for line in status.lines() {
                if line.starts_with("voluntary_ctxt_switches")
                    || line.starts_with("nonvoluntary_ctxt_switches")
                {
                    total += line.split_whitespace().nth(1)?.parse::<u64>().ok()?;
                }
            }
        }
        Some(total)
    }

    pub fn resident_bytes() -> Option<u64> {
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        Some(pages * 4096)
    }

    pub fn cpu_name() -> Option<String> {
        let info = std::fs::read_to_string("/proc/cpuinfo").ok()?;
        let line = info.lines().find(|l| l.starts_with("model name"))?;
        Some(line.split(':').nth(1)?.trim().to_string())
    }

    pub fn os_version() -> Option<String> {
        Some(
            std::fs::read_to_string("/proc/sys/kernel/osrelease")
                .ok()?
                .trim()
                .to_string(),
        )
    }
}

#[cfg(windows)]
mod imp {
    use std::time::Duration;
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

    fn ft(t: FILETIME) -> u64 {
        ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64
    }

    pub fn process_cpu_time() -> Option<Duration> {
        let zero = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let (mut c, mut e, mut k, mut u) = (zero, zero, zero, zero);
        // SAFETY: GetCurrentProcess returns a pseudo-handle that is always
        // valid; the four out-pointers are valid FILETIME locals.
        let ok = unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) };
        if ok == 0 {
            return None;
        }
        // FILETIME is in 100 ns units.
        Some(Duration::from_nanos((ft(k) + ft(u)) * 100))
    }

    pub fn context_switches() -> Option<u64> {
        None
    }

    pub fn resident_bytes() -> Option<u64> {
        // SAFETY: zeroed is a valid bit pattern for this plain C struct.
        let mut pmc: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
        pmc.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        // SAFETY: pseudo-handle as above; `pmc` is valid for `cb` bytes.
        let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut pmc, pmc.cb) };
        (ok != 0).then_some(pmc.WorkingSetSize as u64)
    }

    pub fn cpu_name() -> Option<String> {
        let out = std::process::Command::new("reg")
            .args([
                "query",
                r"HKLM\HARDWARE\DESCRIPTION\System\CentralProcessor\0",
                "/v",
                "ProcessorNameString",
            ])
            .output()
            .ok()?;
        let s = String::from_utf8_lossy(&out.stdout);
        let line = s.lines().find(|l| l.contains("ProcessorNameString"))?;
        Some(line.split("REG_SZ").nth(1)?.trim().to_string())
    }

    pub fn os_version() -> Option<String> {
        let out = std::process::Command::new("cmd")
            .args(["/c", "ver"])
            .output()
            .ok()?;
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod imp {
    use std::time::Duration;
    pub fn process_cpu_time() -> Option<Duration> {
        None
    }
    pub fn context_switches() -> Option<u64> {
        None
    }
    pub fn resident_bytes() -> Option<u64> {
        None
    }
    pub fn cpu_name() -> Option<String> {
        None
    }
    pub fn os_version() -> Option<String> {
        None
    }
}
