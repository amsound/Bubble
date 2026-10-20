//! How much memory prepared audio may use.

/// Share of physical memory prepared audio may occupy; the rest is left for the
/// system, other applications and the peak of a load in progress.
const SHARE: f64 = 0.5;

/// Used when the physical memory size cannot be read.
const FALLBACK_BYTES: u64 = 8_000_000_000;

/// Bytes of prepared audio allowed on this machine.
pub fn audio_budget() -> u64 {
    (physical_memory().unwrap_or(FALLBACK_BYTES * 2) as f64 * SHARE) as u64
}

/// Installed RAM in bytes.
#[cfg(target_os = "macos")]
pub fn physical_memory() -> Option<u64> {
    let mut size: u64 = 0;
    let mut len = std::mem::size_of::<u64>();
    // SAFETY: "hw.memsize" is a u64; `size` and `len` describe a valid buffer.
    let r = unsafe {
        libc::sysctlbyname(
            c"hw.memsize".as_ptr(),
            (&mut size as *mut u64).cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    (r == 0 && size > 0).then_some(size)
}

#[cfg(target_os = "windows")]
pub fn physical_memory() -> Option<u64> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    // SAFETY: MEMORYSTATUSEX is plain data; dwLength must be set before the call.
    unsafe {
        let mut m: MEMORYSTATUSEX = std::mem::zeroed();
        m.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
        (GlobalMemoryStatusEx(&mut m) != 0).then_some(m.ullTotalPhys)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn physical_memory() -> Option<u64> {
    let info = std::fs::read_to_string("/proc/meminfo").ok()?;
    let kb: u64 = info
        .lines()
        .find(|l| l.starts_with("MemTotal:"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    Some(kb * 1024)
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_physical_memory() {
        let m = super::physical_memory().expect("physical memory");
        assert!(m > 512 * 1024 * 1024, "{m}");
        assert!(super::audio_budget() < m);
    }
}
