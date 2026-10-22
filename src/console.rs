//! Windows console handling for the GUI mode (no-op elsewhere).

/// Environment flag set on the detached GUI child, to avoid respawning forever.
const DETACHED_ENV: &str = "STORINGUNICORNS_DETACHED";

/// What `main` should do before starting the GUI.
#[derive(Debug, PartialEq, Eq)]
pub enum GuiLaunch {
    /// Start the GUI in this process.
    Continue,
    /// A detached copy was started; exit now.
    Exit,
}

#[cfg(windows)]
mod sys {
    #[link(name = "kernel32")]
    extern "system" {
        pub fn GetConsoleProcessList(list: *mut u32, count: u32) -> u32;
        pub fn FreeConsole() -> i32;
        pub fn SetStdHandle(std_handle: u32, handle: *mut core::ffi::c_void) -> i32;
    }

    const STD_INPUT_HANDLE: u32 = -10i32 as u32;
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    const STD_ERROR_HANDLE: u32 = -12i32 as u32;

    /// Leave the console, and forget its handles: once it's gone they are
    /// dangling, and `std::process::Command` would fail to hand them to a
    /// child ("The handle is invalid"), e.g. the relaunch after an update.
    pub fn detach_console() {
        unsafe {
            FreeConsole();
            for h in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
                SetStdHandle(h, core::ptr::null_mut());
            }
        }
    }
}

/// Prepare the process for running the GUI.
#[cfg(windows)]
pub fn prepare_gui() -> GuiLaunch {
    use std::os::windows::process::CommandExt;

    if std::env::var_os(DETACHED_ENV).is_some() {
        // Don't leak the flag to our own children (e.g. an updater relaunch),
        // which would then skip detaching.
        std::env::remove_var(DETACHED_ENV);
        sys::detach_console();
        return GuiLaunch::Continue;
    }
    let mut pids = [0u32; 4];
    let attached = unsafe { sys::GetConsoleProcessList(pids.as_mut_ptr(), pids.len() as u32) };
    if attached <= 1 {
        // Our own console (double-click / shortcut): just hide it.
        sys::detach_console();
        return GuiLaunch::Continue;
    }
    // Shared with a shell: start a detached copy so the prompt returns.
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    let spawned = std::env::current_exe().and_then(|exe| {
        std::process::Command::new(exe)
            .args(std::env::args_os().skip(1))
            .env(DETACHED_ENV, "1")
            .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
            .spawn()
    });
    match spawned {
        Ok(_) => GuiLaunch::Exit,
        // Could not detach: run attached rather than not at all.
        Err(_) => GuiLaunch::Continue,
    }
}

#[cfg(not(windows))]
pub fn prepare_gui() -> GuiLaunch {
    GuiLaunch::Continue
}
