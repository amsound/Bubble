//! What happens when Bubble panics: a crash report is written, unsaved work is
//! autosaved if the session can be reached, and the next launch says so.
//!
//! The regular autosave (every 30 s) still covers a crash in which the session is
//! locked by the code that failed.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, TryLockError};

use bubble_control::Controller;

use crate::autosave::{autosave_path, recovery_dir};

/// How an autosave that could not reach the session starts.
pub const BUSY: &str = "session busy";

/// Name of the file pointing at a report the user has not been told about.
const PENDING: &str = "pending";

fn crash_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("app", "Bubble", "Bubble").map(|d| d.data_dir().join("Crashes"))
}

/// Installs the panic hook. Runs on whichever thread panics, including the audio
/// thread, so it takes no lock it might wait on.
pub fn install(ctrl: Arc<Mutex<Controller>>, session_id: u64) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let saved = autosave_now(&ctrl, session_id);
        let report = format!(
            "Bubble {} crashed at {}\n\
             OS: {} {}\n\
             Thread: {}\n\
             {info}\n\
             Unsaved work: {}\n\n\
             Backtrace:\n{}\n",
            env!("CARGO_PKG_VERSION"),
            jiff::Zoned::now().strftime("%Y-%m-%d %H:%M:%S %Z"),
            std::env::consts::OS,
            std::env::consts::ARCH,
            std::thread::current().name().unwrap_or("unnamed"),
            saved,
            std::backtrace::Backtrace::force_capture(),
        );
        write_report(&report_file(), &report);
        previous(info);
        // A panic on the interface thread leaves nothing to run the window: on Windows it
        // could stay on screen, frozen. End it, for the next launch to offer recovery.
        #[cfg(windows)]
        if std::thread::current().name() == Some("main") {
            std::process::exit(101);
        }
    }));
}

/// Where a crash's report goes.
fn report_file() -> Option<PathBuf> {
    let dir = crash_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join(format!(
        "Crash {}.txt",
        jiff::Zoned::now().strftime("%Y-%m-%d %H-%M-%S")
    )))
}

/// Writes a report, for the next launch to mention.
fn write_report(file: &Option<PathBuf>, report: &str) {
    if let (Some(file), Some(dir)) = (file, crash_dir())
        && std::fs::write(file, report).is_ok()
    {
        let _ = std::fs::write(dir.join(PENDING), file.to_string_lossy().as_bytes());
    }
}

/// Crashes a Rust panic does not cover: an access violation, say in a graphics driver,
/// or a stack overflow. The crashed thread may have no stack or locks to spare, so it
/// only wakes a thread waiting for this, which writes a dump for debugging and a report,
/// then tries for the unsaved work. The process ends after that, or after a time limit
/// if the crash left something held.
#[cfg(windows)]
pub fn install_native(ctrl: Arc<Mutex<Controller>>, session_id: u64) {
    use std::sync::atomic::{AtomicIsize, AtomicU32, AtomicUsize, Ordering};
    use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::Debug::{
        EXCEPTION_POINTERS, MINIDUMP_EXCEPTION_INFORMATION, MiniDumpNormal, MiniDumpWithThreadInfo,
        MiniDumpWriteDump, SetUnhandledExceptionFilter,
    };
    use windows_sys::Win32::System::Threading::{
        CreateEventW, GetCurrentProcess, GetCurrentProcessId, GetCurrentThreadId, INFINITE,
        SetEvent, WaitForSingleObject,
    };

    /// How long a crash waits for its report and autosave.
    const LIMIT_MS: u32 = 15_000;
    /// Earlier dumps kept (each a few megabytes at most).
    const DUMPS_KEPT: usize = 5;
    /// EXCEPTION_EXECUTE_HANDLER: end the process, without Windows' own dialog.
    const END: i32 = 1;

    static CRASHED: AtomicIsize = AtomicIsize::new(0);
    static FINISHED: AtomicIsize = AtomicIsize::new(0);
    static POINTERS: AtomicUsize = AtomicUsize::new(0);
    static THREAD: AtomicU32 = AtomicU32::new(0);

    unsafe extern "system" fn filter(info: *const EXCEPTION_POINTERS) -> i32 {
        // Only the first crash reports; another thread's waits for the end.
        let first = POINTERS
            .compare_exchange(0, info as usize, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok();
        // SAFETY: plain calls with the events made at install.
        unsafe {
            if first {
                THREAD.store(GetCurrentThreadId(), Ordering::SeqCst);
                SetEvent(CRASHED.load(Ordering::SeqCst) as HANDLE);
            }
            WaitForSingleObject(FINISHED.load(Ordering::SeqCst) as HANDLE, LIMIT_MS);
        }
        END
    }

    // SAFETY: unnamed auto-reset and manual-reset events, kept for the whole run.
    let (crashed, finished) = unsafe {
        (
            CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()),
            CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()),
        )
    };
    if crashed.is_null() || crashed == INVALID_HANDLE_VALUE || finished.is_null() {
        return;
    }
    CRASHED.store(crashed as isize, Ordering::SeqCst);
    FINISHED.store(finished as isize, Ordering::SeqCst);
    prune_dumps(DUMPS_KEPT);
    let spawned = std::thread::Builder::new()
        .name("bubble-crash-guard".into())
        .spawn(move || {
            // SAFETY: waits on the event made above.
            unsafe { WaitForSingleObject(CRASHED.load(Ordering::SeqCst) as HANDLE, INFINITE) };
            let info = POINTERS.load(Ordering::SeqCst) as *const EXCEPTION_POINTERS;
            let report_to = report_file();
            let dump_to = report_to.as_ref().map(|r| r.with_extension("dmp"));
            let mut dumped = "no";
            if let Some(file) = dump_to.as_ref().and_then(|d| std::fs::File::create(d).ok()) {
                use std::os::windows::io::AsRawHandle;
                let exception = MINIDUMP_EXCEPTION_INFORMATION {
                    ThreadId: THREAD.load(Ordering::SeqCst),
                    ExceptionPointers: info as *mut EXCEPTION_POINTERS,
                    ClientPointers: 0,
                };
                // SAFETY: this process, a file open for writing, and the crash's details
                // while the crashed thread waits.
                let ok = unsafe {
                    MiniDumpWriteDump(
                        GetCurrentProcess(),
                        GetCurrentProcessId(),
                        file.as_raw_handle() as HANDLE,
                        MiniDumpNormal | MiniDumpWithThreadInfo,
                        &exception,
                        std::ptr::null(),
                        std::ptr::null(),
                    )
                };
                if ok != 0 {
                    dumped = "yes";
                }
            }
            // SAFETY: the pointers Windows gave the filter, valid while it waits.
            let (code, address) = unsafe {
                let record = (*info).ExceptionRecord;
                (
                    (*record).ExceptionCode as u32,
                    (*record).ExceptionAddress as usize,
                )
            };
            let report = |saved: &str| {
                format!(
                    "Bubble {} crashed (native) at {}\n\
                     OS: windows {}\n\
                     Exception: {code:#010x} at {address:#x}, thread {}\n\
                     Dump: {dumped}, beside this file\n\
                     Unsaved work: {saved}\n",
                    env!("CARGO_PKG_VERSION"),
                    jiff::Zoned::now().strftime("%Y-%m-%d %H:%M:%S %Z"),
                    std::env::consts::ARCH,
                    THREAD.load(Ordering::SeqCst),
                )
            };
            // The report first: the session or the heap may be what is broken.
            write_report(&report_to, &report("not reached"));
            let saved = autosave_now(&ctrl, session_id);
            write_report(&report_to, &report(&saved));
            crate::log::info(&format!(
                "NATIVE CRASH {code:#010x} at {address:#x}; unsaved work: {saved}"
            ));
            // SAFETY: the event made above.
            unsafe { SetEvent(FINISHED.load(Ordering::SeqCst) as HANDLE) };
        });
    if spawned.is_ok() {
        // SAFETY: a function that stays valid for the whole run.
        unsafe { SetUnhandledExceptionFilter(Some(filter)) };
    }
}

/// Removes all but the newest dumps (the time in the name sorts them).
#[cfg(windows)]
fn prune_dumps(keep: usize) {
    let Some(entries) = crash_dir().and_then(|d| std::fs::read_dir(d).ok()) else {
        return;
    };
    let mut dumps: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "dmp"))
        .collect();
    dumps.sort();
    let extra = dumps.len().saturating_sub(keep);
    for d in &dumps[..extra] {
        let _ = std::fs::remove_file(d);
    }
}

/// Writes the session's unsaved changes where the next launch looks for them.
pub fn autosave_now(ctrl: &Mutex<Controller>, session_id: u64) -> String {
    let c = match ctrl.try_lock() {
        Ok(c) => c,
        Err(TryLockError::Poisoned(p)) => p.into_inner(),
        Err(TryLockError::WouldBlock) => {
            return format!("{BUSY}; the last regular autosave is offered for recovery");
        }
    };
    if !c.is_dirty() {
        return "none".into();
    }
    let json = bubble_project::io::to_json_string(c.project());
    let target = match c.path() {
        Some(p) => Some(autosave_path(p)),
        None => recovery_dir().map(|d| d.join(format!("Untitled-{session_id}.bubble"))),
    };
    drop(c);
    match target {
        Some(t) => match bubble_project::io::write_atomic(&t, json.as_bytes(), false) {
            Ok(()) => format!("autosaved to {}", t.display()),
            Err(e) => format!("autosave failed: {e}"),
        },
        None => "no place to autosave".into(),
    }
}

/// The report of a crash the user has not been told about yet (told once).
pub fn take_unreported() -> Option<PathBuf> {
    let pending = crash_dir()?.join(PENDING);
    let path = std::fs::read_to_string(&pending).ok()?;
    let _ = std::fs::remove_file(&pending);
    let path = PathBuf::from(path.trim());
    path.exists().then_some(path)
}
