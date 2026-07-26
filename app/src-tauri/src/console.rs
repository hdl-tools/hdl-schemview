//! Make this GUI-subsystem process's stdout/stderr visible when it was launched
//! from a terminal (#240).
//!
//! Release builds set `windows_subsystem = "windows"` (`main.rs`) so a
//! double-click never flashes a console window. The cost is that Windows gives
//! the process **no standard handles at all**, so every `println!`/`eprintln!`
//! silently goes nowhere — which already broke the documented CLI contract
//! (`-h` → usage + exit 0, a usage error → stderr + exit 2) long before
//! `--bench` needed to print anything.
//!
//! Deliberately **dependency-free**, hand-declaring the four kernel32 entry
//! points, matching the precedent set by `scale-bench`'s `mem.rs` RSS probes:
//! a packaging change should not add a dependency to the product binary.
//!
//! Note this only affects the *parent*. A child spawned by `--bench` receives
//! explicit pipe handles through `STARTUPINFO` and needs none of this.

/// Best-effort: attach to the console this process was launched from so its
/// stdio is visible. Safe to call unconditionally — a no-op off Windows, and a
/// no-op when there is no parent console (a double-click), which is why it
/// never calls `AllocConsole`: that would pop a window on a GUI launch.
///
/// Call before any output, i.e. first thing in `main`.
pub fn attach_parent_console() {
    imp::attach();
}

#[cfg(windows)]
mod imp {
    use core::ffi::c_void;
    use core::ptr::null_mut;

    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    const STD_ERROR_HANDLE: u32 = -12i32 as u32;
    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const OPEN_EXISTING: u32 = 3;

    fn invalid_handle() -> *mut c_void {
        -1isize as *mut c_void
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn AttachConsole(dwProcessId: u32) -> i32;
        fn GetStdHandle(nStdHandle: u32) -> *mut c_void;
        fn SetStdHandle(nStdHandle: u32, hHandle: *mut c_void) -> i32;
        fn CreateFileW(
            lpFileName: *const u16,
            dwDesiredAccess: u32,
            dwShareMode: u32,
            lpSecurityAttributes: *mut c_void,
            dwCreationDisposition: u32,
            dwFlagsAndAttributes: u32,
            hTemplateFile: *mut c_void,
        ) -> *mut c_void;
    }

    /// A handle Windows actually gave us. `null` is "this subsystem has no such
    /// handle"; `INVALID_HANDLE_VALUE` is the error sentinel.
    fn usable(h: *mut c_void) -> bool {
        !h.is_null() && h != invalid_handle()
    }

    pub fn attach() {
        // SAFETY: every call below is a plain kernel32 FFI call with owned or
        // null arguments; none of the returned handles are freed here (they
        // live for the process, which is what SetStdHandle requires).
        unsafe {
            let out_ok = usable(GetStdHandle(STD_OUTPUT_HANDLE));
            let err_ok = usable(GetStdHandle(STD_ERROR_HANDLE));
            // Both already usable ⇒ the shell handed us pipes (`app.exe > out.txt`,
            // or a debug console-subsystem build). Attaching would be pointless
            // and overwriting them would break the redirection.
            if out_ok && err_ok {
                return;
            }
            if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
                return; // No parent console — launched from Explorer.
            }
            // AttachConsole gives the process a console but does *not* populate
            // its std handles, so open the console's own device explicitly —
            // only for the streams that weren't already redirected.
            if !out_ok {
                redirect(STD_OUTPUT_HANDLE);
            }
            if !err_ok {
                redirect(STD_ERROR_HANDLE);
            }
        }
    }

    unsafe fn redirect(which: u32) {
        // "CONOUT$", NUL-terminated UTF-16.
        const CONOUT: [u16; 8] = [
            b'C' as u16,
            b'O' as u16,
            b'N' as u16,
            b'O' as u16,
            b'U' as u16,
            b'T' as u16,
            b'$' as u16,
            0,
        ];
        let h = CreateFileW(
            CONOUT.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null_mut(),
            OPEN_EXISTING,
            0,
            null_mut(),
        );
        if usable(h) {
            SetStdHandle(which, h);
        }
    }
}

#[cfg(not(windows))]
mod imp {
    /// Linux and macOS give a GUI process ordinary inherited stdio already.
    pub fn attach() {}
}
