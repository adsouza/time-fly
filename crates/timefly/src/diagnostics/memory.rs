//! Process memory and OS description, per platform: the footprint the benches gate, the split
//! the hop-memory trace logs ([`super::hop_memory`]), and the web's heap and linear memory.

#[cfg(target_os = "macos")]
pub const FOOTPRINT_KIND: &str = "phys_footprint, includes GPU allocations on Apple silicon";
#[cfg(all(unix, not(target_os = "macos")))]
pub const FOOTPRINT_KIND: &str = "resident set; GPU memory not included";
#[cfg(not(unix))]
pub const FOOTPRINT_KIND: &str = "not available here";

#[cfg(target_os = "macos")]
fn rusage() -> Option<libc::rusage_info_v4> {
    // SAFETY: `proc_pid_rusage` fills the struct for the flavour asked for; it is plain data.
    unsafe {
        let mut info: libc::rusage_info_v4 = std::mem::zeroed();
        let ok = libc::proc_pid_rusage(
            std::process::id() as libc::c_int,
            libc::RUSAGE_INFO_V4,
            (&raw mut info).cast(),
        );
        (ok == 0).then_some(info)
    }
}

/// The process's current memory footprint in bytes.
pub fn footprint_now() -> Option<u64> {
    #[cfg(target_os = "macos")]
    return rusage().map(|r| r.ri_phys_footprint);
    #[cfg(all(unix, not(target_os = "macos")))]
    return std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|s| {
            let pages: u64 = s.split_whitespace().nth(1)?.parse().ok()?;
            // SAFETY: sysconf has no preconditions.
            let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
            Some(pages * page.max(0) as u64)
        });
    #[cfg(not(unix))]
    None
}

/// The process's peak memory footprint in bytes.
pub fn footprint_peak() -> Option<u64> {
    #[cfg(target_os = "macos")]
    return rusage().map(|r| r.ri_lifetime_max_phys_footprint);
    #[cfg(all(unix, not(target_os = "macos")))]
    // SAFETY: getrusage fills a plain-data struct.
    return unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        (libc::getrusage(libc::RUSAGE_SELF, &mut usage) == 0)
            .then(|| usage.ru_maxrss.max(0) as u64 * 1024)
    };
    #[cfg(not(unix))]
    None
}

/// The OS name and version.
pub fn os_description() -> String {
    #[cfg(target_os = "macos")]
    {
        let version = sysctl_string(c"kern.osproductversion").unwrap_or_default();
        let build = sysctl_string(c"kern.osversion").unwrap_or_default();
        format!("macOS {version} ({build}), {}", std::env::consts::ARCH)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let pretty = std::fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|s| {
                s.lines().find_map(|l| {
                    l.strip_prefix("PRETTY_NAME=")
                        .map(|v| v.trim_matches('"').to_string())
                })
            })
            .unwrap_or_else(|| std::env::consts::OS.to_string());
        let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        format!("{pretty}, kernel {kernel}, {}", std::env::consts::ARCH)
    }
    #[cfg(target_arch = "wasm32")]
    {
        web_sys::window()
            .and_then(|w| w.navigator().user_agent().ok())
            .map_or_else(|| "web".to_string(), |ua| format!("web: {ua}"))
    }
    #[cfg(not(any(unix, target_arch = "wasm32")))]
    {
        std::env::consts::OS.to_string()
    }
}

/// Web: the JS heap in use (`performance.memory.usedJSHeapSize`, Chrome only) and the size
/// of the wasm linear memory (which grows but never shrinks).
#[cfg(target_arch = "wasm32")]
pub fn web_memory() -> (Option<u64>, Option<u64>) {
    use wasm_bindgen::JsValue;
    let get = |object: &JsValue, key: &str| {
        js_sys::Reflect::get(object, &JsValue::from_str(key))
            .ok()
            .filter(|v| !v.is_undefined() && !v.is_null())
    };
    let bytes = |v: JsValue| v.as_f64().filter(|b| *b >= 0.0).map(|b| b as u64);
    let js_heap = web_sys::window()
        .and_then(|w| w.performance())
        .and_then(|p| get(&JsValue::from(p), "memory"))
        .and_then(|m| get(&m, "usedJSHeapSize"))
        .and_then(bytes);
    let linear = get(&wasm_bindgen::memory(), "buffer")
        .and_then(|b| get(&b, "byteLength"))
        .and_then(bytes);
    (js_heap, linear)
}

/// One reading of the process's memory as the kernel and malloc split it (macOS only), for
/// the hop-memory trace ([`super::hop_memory`] and `crate::bench::journey::trace`).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct MemorySplit {
    /// `phys_footprint`: what the bench gates.
    pub footprint: u64,
    /// Dirty anonymous memory: malloc's pages, and anything else the process maps privately.
    pub internal: u64,
    pub compressed: u64,
    /// Pages malloc marked reusable (freed, not counted in the footprint).
    pub reusable: u64,
    /// Memory tagged as graphics (Metal's textures and buffers), in the footprint.
    pub graphics: u64,
    /// Non-volatile purgeable memory (some driver allocations), in the footprint.
    pub purgeable: u64,
    /// malloc's bytes in live blocks, all zones.
    pub malloc_in_use: u64,
    /// The bytes malloc's zones hold from the kernel, live or free.
    pub malloc_held: u64,
}

/// The task's VM info, rev 7 (`<mach/task_info.h>`, packed to 4 bytes).
#[cfg(target_os = "macos")]
#[repr(C, packed(4))]
#[derive(Copy, Clone)]
pub(crate) struct TaskVmInfo {
    virtual_size: u64,
    region_count: i32,
    page_size: i32,
    resident_size: u64,
    resident_size_peak: u64,
    device: u64,
    device_peak: u64,
    internal: u64,
    internal_peak: u64,
    external: u64,
    external_peak: u64,
    reusable: u64,
    reusable_peak: u64,
    purgeable_volatile_pmap: u64,
    purgeable_volatile_resident: u64,
    purgeable_volatile_virtual: u64,
    compressed: u64,
    compressed_peak: u64,
    compressed_lifetime: u64,
    phys_footprint: u64,
    min_address: u64,
    max_address: u64,
    /// rev 3: 21 ledgers, from `ledger_phys_footprint_peak` to
    /// `ledger_tag_neural_nofootprint_compressed`; graphics footprint is index 13.
    ledgers: [i64; 21],
    limit_bytes_remaining: u64,
    decompressions: i32,
    ledger_swapins: i64,
    neural_nofootprint_total: i64,
    neural_nofootprint_peak: i64,
}

/// The process's memory split ([`MemorySplit`]), or `None` off macOS or on a failed call.
pub fn split() -> Option<MemorySplit> {
    #[cfg(target_os = "macos")]
    {
        const TASK_VM_INFO: libc::task_flavor_t = 22;
        const PURGEABLE_NONVOLATILE: usize = 1;
        const GRAPHICS_FOOTPRINT: usize = 13;
        // SAFETY: `task_info` writes at most `count` natural_t words into the plain-data
        // struct and updates `count`; `malloc_zone_statistics` with a null zone sums all
        // zones into the plain-data struct.
        unsafe {
            let mut vm: TaskVmInfo = std::mem::zeroed();
            let mut count = (size_of::<TaskVmInfo>() / size_of::<libc::natural_t>())
                as libc::mach_msg_type_number_t;
            #[expect(deprecated, reason = "libc points at the mach2 crate; one call")]
            let task = libc::mach_task_self();
            let ok = libc::task_info(task, TASK_VM_INFO, (&raw mut vm).cast(), &mut count);
            if ok != libc::KERN_SUCCESS {
                return None;
            }
            let mut stats: libc::malloc_statistics_t = std::mem::zeroed();
            libc::malloc_zone_statistics(std::ptr::null_mut(), &mut stats);
            let ledgers = vm.ledgers;
            Some(MemorySplit {
                footprint: vm.phys_footprint,
                internal: vm.internal,
                compressed: vm.compressed,
                reusable: vm.reusable,
                graphics: ledgers[GRAPHICS_FOOTPRINT].max(0) as u64,
                purgeable: ledgers[PURGEABLE_NONVOLATILE].max(0) as u64,
                malloc_in_use: stats.size_in_use as u64,
                malloc_held: stats.size_allocated as u64,
            })
        }
    }
    #[cfg(not(target_os = "macos"))]
    None
}

#[cfg(target_os = "macos")]
fn sysctl_string(name: &std::ffi::CStr) -> Option<String> {
    let mut buf = [0u8; 128];
    let mut len = buf.len();
    // SAFETY: the buffer and its length are valid; sysctlbyname writes at most `len` bytes.
    let ok = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            buf.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    (ok == 0).then(|| {
        String::from_utf8_lossy(&buf[..len.min(buf.len())])
            .trim_end_matches('\0')
            .to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn the_memory_split_reads_the_kernels_footprint() {
        // The packed struct is rev 7's 93 natural_t words.
        assert_eq!(size_of::<TaskVmInfo>(), 93 * 4);
        let before = split().expect("task_info and malloc statistics");
        let rusage = footprint_now().expect("the footprint");
        // The same ledger read twice: within a few MiB of allocation between the calls.
        assert!(
            before.footprint.abs_diff(rusage) < 8 << 20,
            "{before:?} vs {rusage}"
        );
        assert!(before.internal > 0 && before.internal <= before.footprint + (64 << 20));
        // A 64 MiB block written through shows up as in use, then as held.
        let block = vec![1u8; 64 << 20];
        let with = split().unwrap();
        assert!(
            with.malloc_in_use >= before.malloc_in_use + (60 << 20),
            "{with:?}"
        );
        assert!(with.malloc_held >= with.malloc_in_use);
        drop(block);
    }
}
