//! In-process Windows console input, encoding, and Ctrl+C hook for `hlds.exe`.
//!
//! # Problem
//! 1. `hlds.exe` (ReHLDS dedicated server console) initializes its CRT locale to `.UTF-8`
//!    and sets console output code page to `CP_UTF8` (65001).
//! 2. For keyboard input, `hlds.exe` calls `ReadConsoleInputA` rather than `ReadConsoleInputW`.
//!    In Windows ConHost, single-byte ANSI input cannot reliably deliver multi-byte UTF-8
//!    characters, resulting in `?` substitution or character loss.
//! 3. In `.UTF-8` CRT locale, `isprint(ch)` returns `0` (false) for all bytes `>= 128` (0x80..=0xFF).
//!    Because `hlds.exe` filters all keyboard input through `isprint(ch)` and re-validates the
//!    entire command line with `isprint(ch)` on Enter, any Cyrillic or non-ASCII characters are
//!    silently dropped or rejected.
//! 4. When pressing Ctrl+C in a console window, Windows terminates threads abruptly, causing
//!    `steamclient.dll` / `tier0.dll` worker thread assertions:
//!    `src\tier0\threadtools.cpp (3807) : Assertion Failed: Illegal termination of worker thread 'CFileWriterThread'`.
//!
//! # Solution
//! We hook `hlds.exe`'s Import Address Table (IAT) in-process and register a console control handler:
//! 1. `isprint`: Accept all bytes in `0x80..=0xFF` as printable, preserving UTF-8 multi-byte sequences.
//! 2. `ReadConsoleInputA` / `PeekConsoleInputA`: Read true Unicode key events via `ReadConsoleInputW`,
//!    encode non-ASCII characters into UTF-8 byte streams, and feed them byte-by-byte into `hlds.exe`.
//!    On Backspace (`VK_BACK`), multi-byte character lengths are tracked so that the exact number
//!    of backspaces is dispatched to delete the full character.
//! 3. `SetConsoleCtrlHandler`: Catch `CTRL_C_EVENT` / `CTRL_BREAK_EVENT`, issue `"quit\n"` to the engine
//!    for clean graceful server shutdown without thread assertions, and return `TRUE`.

#[cfg(windows)]
mod imp {
    use std::collections::VecDeque;
    use std::ffi::c_void;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicPtr, Ordering};
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::Console::{
        GetStdHandle, INPUT_RECORD, KEY_EVENT, PeekConsoleInputW, ReadConsoleInputW,
        STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleCP, SetConsoleCtrlHandler,
        SetConsoleOutputCP, WriteConsoleA, WriteConsoleInputW,
    };
    use windows_sys::Win32::System::Diagnostics::Debug::FlushInstructionCache;
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
    use windows_sys::Win32::System::Memory::{
        PAGE_EXECUTE_READWRITE, PAGE_READWRITE, VirtualProtect,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, TerminateProcess};

    /// Function pointer for sending commands to the engine on shutdown.
    static SHUTDOWN_COMMAND_FN: AtomicPtr<()> = AtomicPtr::new(std::ptr::null_mut());

    /// Flag to detect repeated Ctrl+C presses for instant force-quit.
    static SHUTDOWN_TRIGGERED: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);

    /// Queued extra input records (e.g. subsequent bytes of a multi-byte UTF-8 sequence, or extra backspaces).
    static QUEUED_INPUTS: Mutex<VecDeque<INPUT_RECORD>> = Mutex::new(VecDeque::new());

    /// Track byte lengths of recently typed characters so that backspace deletes the full character.
    static EMITTED_CHAR_LEN: Mutex<Vec<u8>> = Mutex::new(Vec::new());

    type FnReadConsoleInputA =
        unsafe extern "system" fn(HANDLE, *mut INPUT_RECORD, u32, *mut u32) -> i32;
    type FnPeekConsoleInputA =
        unsafe extern "system" fn(HANDLE, *mut INPUT_RECORD, u32, *mut u32) -> i32;
    type FnIsPrint = unsafe extern "C" fn(i32) -> i32;

    type SpewOutputFuncFn = unsafe extern "C" fn(i32, *const std::os::raw::c_char) -> i32;
    type FnSpewOutputFunc = unsafe extern "C" fn(Option<SpewOutputFuncFn>);
    type FnGetSpewOutputFunc = unsafe extern "C" fn() -> Option<SpewOutputFuncFn>;
    type FnSetAssertDialogsShown = unsafe extern "C" fn(i32);

    static ORIG_SPEW_FUNC: AtomicPtr<()> = AtomicPtr::new(std::ptr::null_mut());

    static mut ORIG_READ_CONSOLE_INPUT_A: Option<FnReadConsoleInputA> = None;
    static mut ORIG_PEEK_CONSOLE_INPUT_A: Option<FnPeekConsoleInputA> = None;
    static mut ORIG_ISPRINT: Option<FnIsPrint> = None;

    /// Filter out teardown assertions and worker pool cleanup messages during server shutdown.
    unsafe extern "C" fn filter_spew_output(
        spew_type: i32,
        msg: *const std::os::raw::c_char,
    ) -> i32 {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if !msg.is_null() {
                let cstr = unsafe { std::ffi::CStr::from_ptr(msg) };
                let bytes = cstr.to_bytes();
                let is_shutdown_noise = bytes
                    .windows(b"Illegal termination of worker thread".len())
                    .any(|w| w == b"Illegal termination of worker thread")
                    || bytes
                        .windows(b"CWorkThreadPool".len())
                        .any(|w| w == b"CWorkThreadPool")
                    || (bytes
                        .windows(b"Assertion Failed".len())
                        .any(|w| w == b"Assertion Failed")
                        && bytes
                            .windows(b"threadtools.cpp".len())
                            .any(|w| w == b"threadtools.cpp"));

                if is_shutdown_noise {
                    // SPEW_CONTINUE (1): suppress output and prevent debugger break / assertion failure
                    return 1;
                }
            }

            let orig_ptr = ORIG_SPEW_FUNC.load(Ordering::Relaxed);
            if !orig_ptr.is_null() {
                let orig_fn: SpewOutputFuncFn = unsafe { std::mem::transmute(orig_ptr) };
                return unsafe { orig_fn(spew_type, msg) };
            }

            1
        }));

        result.unwrap_or(1)
    }

    /// Patch function entry point with `mov al, 1; ret` (3 bytes: 0xB0, 0x01, 0xC3).
    /// Returns 1 immediately to caller, satisfying `__cdecl` and skipping assertions / int 3.
    unsafe fn patch_return_one(proc: *const c_void) {
        if proc.is_null() {
            return;
        }
        let mut old_protect = 0;
        if unsafe {
            VirtualProtect(
                proc as *mut c_void,
                3,
                PAGE_EXECUTE_READWRITE,
                &mut old_protect,
            )
        } != 0
        {
            let bytes = proc as *mut u8;
            unsafe {
                *bytes = 0xb0;
                *bytes.add(1) = 0x01;
                *bytes.add(2) = 0xc3;
                VirtualProtect(proc as *mut c_void, 3, old_protect, &mut old_protect);
                FlushInstructionCache(GetCurrentProcess(), proc, 3);
            }
        }
    }

    /// Patch function entry point with `ret` (1 byte: 0xC3).
    unsafe fn patch_return_void(proc: *const c_void) {
        if proc.is_null() {
            return;
        }
        let mut old_protect = 0;
        if unsafe {
            VirtualProtect(
                proc as *mut c_void,
                1,
                PAGE_EXECUTE_READWRITE,
                &mut old_protect,
            )
        } != 0
        {
            let bytes = proc as *mut u8;
            unsafe {
                *bytes = 0xc3;
                VirtualProtect(proc as *mut c_void, 1, old_protect, &mut old_protect);
                FlushInstructionCache(GetCurrentProcess(), proc, 1);
            }
        }
    }

    /// Suppress Valve tier0 assertions and teardown warnings across all loaded tier0/vstdlib modules.
    pub fn suppress_tier0_spew() {
        let candidate_modules: [&[u8]; 5] = [
            b"tier0.dll\0",
            b"tier0_s.dll\0",
            b"tier0_s64.dll\0",
            b"vstdlib.dll\0",
            b"vstdlib_s.dll\0",
        ];

        for mod_name in candidate_modules {
            let h_mod = unsafe { GetModuleHandleA(mod_name.as_ptr()) };
            if !h_mod.is_null() {
                // 1. Disable assertion dialog boxes
                let set_dialogs_proc = unsafe {
                    GetProcAddress(h_mod, c"SetAssertDialogsShown".as_ptr() as *const u8)
                };
                if let Some(set_dialogs_fn) = set_dialogs_proc {
                    let set_dialogs: FnSetAssertDialogsShown =
                        unsafe { std::mem::transmute(set_dialogs_fn) };
                    unsafe { set_dialogs(0) };
                }

                // 2. Retrieve original spew function if not captured yet
                let get_spew_proc =
                    unsafe { GetProcAddress(h_mod, c"GetSpewOutputFunc".as_ptr() as *const u8) };
                if let Some(get_spew_fn) = get_spew_proc {
                    let get_spew: FnGetSpewOutputFunc = unsafe { std::mem::transmute(get_spew_fn) };
                    if let Some(prev) = unsafe { get_spew() } {
                        let prev_ptr = prev as *mut ();
                        if prev_ptr != filter_spew_output as *mut () {
                            ORIG_SPEW_FUNC.store(prev_ptr, Ordering::Relaxed);
                        }
                    }
                }

                // 3. Install filter
                let spew_proc =
                    unsafe { GetProcAddress(h_mod, c"SpewOutputFunc".as_ptr() as *const u8) };
                if let Some(spew_fn) = spew_proc {
                    let set_spew: FnSpewOutputFunc = unsafe { std::mem::transmute(spew_fn) };
                    unsafe { set_spew(Some(filter_spew_output)) };
                }

                // 4. Neutralize assertion handlers at entry point
                let funcs_to_return_one: [&[u8]; 6] = [
                    b"AssertMsgImplementationF\0",
                    b"AssertMsgImplementationPreformatted\0",
                    b"AssertMsgImplementationV\0",
                    b"?AssertFailed@?$AssertMsgHelper@$00@@SA_NPBDI0@Z\0",
                    b"?AssertFailed@?$AssertMsgHelper@$0A@@@SA_NPBDI0ZZ\0",
                    b"DoNewAssertDialog\0",
                ];
                for fn_name in funcs_to_return_one {
                    if let Some(proc) = unsafe { GetProcAddress(h_mod, fn_name.as_ptr()) } {
                        unsafe { patch_return_one(proc as *const c_void) };
                    }
                }

                let funcs_to_return_void: [&[u8]; 1] = [b"_ExitOnFatalAssert\0"];
                for fn_name in funcs_to_return_void {
                    if let Some(proc) = unsafe { GetProcAddress(h_mod, fn_name.as_ptr()) } {
                        unsafe { patch_return_void(proc as *const c_void) };
                    }
                }
            }
        }
    }

    /// Register engine server command function for graceful Ctrl+C shutdown.
    pub fn register_shutdown_handler(cmd_fn: unsafe extern "C" fn(*const std::os::raw::c_char)) {
        SHUTDOWN_COMMAND_FN.store(cmd_fn as *mut (), Ordering::SeqCst);
    }

    /// Windows Console Control Handler (`Ctrl+C`, `Ctrl+Break`, window close).
    unsafe extern "system" fn console_ctrl_handler(ctrl_type: u32) -> i32 {
        match ctrl_type {
            // CTRL_C_EVENT (0), CTRL_BREAK_EVENT (1), CTRL_CLOSE_EVENT (2)
            0..=2 => {
                suppress_tier0_spew();

                // On repeated Ctrl+C, force immediate exit without waiting
                if SHUTDOWN_TRIGGERED.swap(true, Ordering::SeqCst) {
                    suppress_tier0_spew();
                    unsafe {
                        // TerminateProcess terminates without invoking CRT static destructors that assert
                        TerminateProcess(GetCurrentProcess(), 0);
                    }
                    return 1;
                }

                // 1. Issue "quit\n" to engine command buffer
                let ptr = SHUTDOWN_COMMAND_FN.load(Ordering::SeqCst);
                if !ptr.is_null() {
                    // SAFETY: ptr was registered from a valid engine ServerCommand function pointer.
                    unsafe {
                        let cmd_fn: unsafe extern "C" fn(*const std::os::raw::c_char) =
                            std::mem::transmute(ptr);
                        cmd_fn(c"quit\n".as_ptr());
                    }
                }

                // 2. Also inject "quit\r" into hooked console input queue
                {
                    let mut queued = match QUEUED_INPUTS.lock() {
                        Ok(q) => q,
                        Err(e) => e.into_inner(),
                    };
                    for &b in b"quit\r" {
                        let mut rec: INPUT_RECORD = unsafe { std::mem::zeroed() };
                        rec.EventType = KEY_EVENT as u16;
                        rec.Event.KeyEvent.bKeyDown = 1;
                        rec.Event.KeyEvent.wRepeatCount = 1;
                        rec.Event.KeyEvent.uChar.AsciiChar = b as i8;
                        queued.push_back(rec);
                    }
                }

                // 3. Wake up ReadConsoleInput if engine is blocked on input
                let h_in = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
                if !h_in.is_null() {
                    let mut dummy: INPUT_RECORD = unsafe { std::mem::zeroed() };
                    dummy.EventType = KEY_EVENT as u16;
                    dummy.Event.KeyEvent.bKeyDown = 1;
                    dummy.Event.KeyEvent.wRepeatCount = 1;
                    dummy.Event.KeyEvent.wVirtualKeyCode = 13;
                    dummy.Event.KeyEvent.uChar.AsciiChar = 13;
                    let mut written: u32 = 0;
                    unsafe {
                        WriteConsoleInputW(h_in, &dummy, 1, &mut written);
                    }
                }

                // 4. Output user-friendly notice to console
                let h_out = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
                if !h_out.is_null() {
                    let msg = b"\n[GoldSrc] Shutdown initiated (Ctrl+C). Press Ctrl+C again to force exit.\n";
                    let mut written: u32 = 0;
                    unsafe {
                        WriteConsoleA(
                            h_out,
                            msg.as_ptr(),
                            msg.len() as u32,
                            &mut written,
                            std::ptr::null_mut(),
                        );
                    }
                }

                // 5. Watchdog fallback timer (10 seconds): allows full graceful engine & Steam
                // shutdown. If engine deadlocks beyond 10s, cleanly terminate via TerminateProcess.
                std::thread::spawn(|| {
                    std::thread::sleep(std::time::Duration::from_secs(10));
                    suppress_tier0_spew();
                    unsafe {
                        TerminateProcess(GetCurrentProcess(), 0);
                    }
                });

                // Return TRUE (1) so default OS handler does not terminate abruptly
                1
            }
            _ => 0,
        }
    }

    /// Intercepted `isprint` function.
    ///
    /// Considers any byte in `0x80..=0xFF` printable so that UTF-8 code units (and Cyrillic ANSI)
    /// pass input filtering and command line validation in `hlds.exe`.
    unsafe extern "C" fn hooked_isprint(c: i32) -> i32 {
        let uc = c as u32;
        if (0x80..=0xFF).contains(&uc) {
            return 1;
        }
        if (0x20..=0x7E).contains(&uc) {
            return 1;
        }
        0
    }

    /// Intercepted `ReadConsoleInputA` function.
    ///
    /// Reads wide characters using `ReadConsoleInputW`, converts Unicode characters to UTF-8,
    /// and feeds the bytes into `hlds.exe`.
    unsafe extern "system" fn hooked_read_console_input_a(
        h_console_input: HANDLE,
        lp_buffer: *mut INPUT_RECORD,
        n_length: u32,
        lp_number_of_events_read: *mut u32,
    ) -> i32 {
        if n_length == 0 || lp_buffer.is_null() || lp_number_of_events_read.is_null() {
            return 0;
        }

        // 1. Drain any pending queued events first (e.g. continuation bytes of a multi-byte sequence)
        let mut queued = match QUEUED_INPUTS.lock() {
            Ok(q) => q,
            Err(e) => e.into_inner(),
        };

        if let Some(item) = queued.pop_front() {
            // SAFETY: lp_buffer is valid for n_length >= 1 records as checked above.
            unsafe {
                *lp_buffer = item;
                *lp_number_of_events_read = 1;
            }
            return 1;
        }
        drop(queued);

        // 2. Read next wide console event via ReadConsoleInputW
        let mut raw_record = std::mem::MaybeUninit::<INPUT_RECORD>::uninit();
        let mut raw_read: u32 = 0;
        // SAFETY: raw_record is a valid 1-element buffer allocated on the stack.
        let ok = unsafe {
            ReadConsoleInputW(h_console_input, raw_record.as_mut_ptr(), 1, &mut raw_read)
        };
        if ok == 0 || raw_read == 0 {
            // SAFETY: lp_number_of_events_read is non-null.
            unsafe {
                *lp_number_of_events_read = 0;
            }
            return ok;
        }

        // SAFETY: ReadConsoleInputW succeeded and wrote 1 record.
        let mut rec = unsafe { raw_record.assume_init() };

        if rec.EventType != KEY_EVENT as u16 {
            // Pass non-keyboard events directly.
            // SAFETY: lp_buffer is valid for writing.
            unsafe {
                *lp_buffer = rec;
                *lp_number_of_events_read = 1;
            }
            return 1;
        }

        // Inspect KEY_EVENT_RECORD
        let b_key_down = unsafe { rec.Event.KeyEvent.bKeyDown };
        if b_key_down == 0 {
            // Key release: clear character byte and forward
            unsafe {
                rec.Event.KeyEvent.uChar.AsciiChar = 0;
                *lp_buffer = rec;
                *lp_number_of_events_read = 1;
            }
            return 1;
        }

        let unicode_char = unsafe { rec.Event.KeyEvent.uChar.UnicodeChar };

        // Handle Backspace (VK_BACK / \x08)
        if unicode_char == 8 {
            let mut history = match EMITTED_CHAR_LEN.lock() {
                Ok(h) => h,
                Err(e) => e.into_inner(),
            };
            let char_len = history.pop().unwrap_or(1);
            drop(history);

            if char_len > 1 {
                let mut queued = match QUEUED_INPUTS.lock() {
                    Ok(q) => q,
                    Err(e) => e.into_inner(),
                };
                // Queue extra backspace events so all bytes of the multi-byte character are removed
                for _ in 0..(char_len - 1) {
                    queued.push_back(rec);
                }
            }

            // SAFETY: lp_buffer is valid for writing.
            unsafe {
                *lp_buffer = rec;
                *lp_number_of_events_read = 1;
            }
            return 1;
        }

        // Handle Enter (\r / 13)
        if unicode_char == 13 {
            let mut history = match EMITTED_CHAR_LEN.lock() {
                Ok(h) => h,
                Err(e) => e.into_inner(),
            };
            history.clear();
            drop(history);

            // SAFETY: lp_buffer is valid for writing.
            unsafe {
                rec.Event.KeyEvent.uChar.AsciiChar = 13;
                *lp_buffer = rec;
                *lp_number_of_events_read = 1;
            }
            return 1;
        }

        // Non-character or ASCII control/navigation key
        if unicode_char == 0 || (unicode_char < 0x20 && unicode_char != 9) {
            // SAFETY: lp_buffer is valid for writing.
            unsafe {
                rec.Event.KeyEvent.uChar.AsciiChar = unicode_char as i8;
                *lp_buffer = rec;
                *lp_number_of_events_read = 1;
            }
            return 1;
        }

        // ASCII character (0x20..=0x7E or Tab)
        if unicode_char <= 0x7F {
            if unicode_char >= 0x20 {
                let mut history = match EMITTED_CHAR_LEN.lock() {
                    Ok(h) => h,
                    Err(e) => e.into_inner(),
                };
                history.push(1);
            }
            // SAFETY: lp_buffer is valid for writing.
            unsafe {
                rec.Event.KeyEvent.uChar.AsciiChar = unicode_char as i8;
                *lp_buffer = rec;
                *lp_number_of_events_read = 1;
            }
            return 1;
        }

        // Non-ASCII Unicode character (Cyrillic, umlauts, etc.): Encode into UTF-8!
        if let Some(ch) = char::from_u32(unicode_char as u32) {
            let mut utf8_buf = [0u8; 4];
            let encoded = ch.encode_utf8(&mut utf8_buf).as_bytes();

            let mut history = match EMITTED_CHAR_LEN.lock() {
                Ok(h) => h,
                Err(e) => e.into_inner(),
            };
            history.push(encoded.len() as u8);
            drop(history);

            // First byte returned immediately
            unsafe {
                rec.Event.KeyEvent.uChar.AsciiChar = encoded[0] as i8;
                *lp_buffer = rec;
                *lp_number_of_events_read = 1;
            }

            // Subsequent bytes queued
            if encoded.len() > 1 {
                let mut queued = match QUEUED_INPUTS.lock() {
                    Ok(q) => q,
                    Err(e) => e.into_inner(),
                };
                for &b in &encoded[1..] {
                    let mut extra = rec;
                    extra.Event.KeyEvent.uChar.AsciiChar = b as i8;
                    queued.push_back(extra);
                }
            }

            return 1;
        }

        // Fallback for unmapped characters
        // SAFETY: lp_buffer is valid for writing.
        unsafe {
            *lp_buffer = rec;
            *lp_number_of_events_read = 1;
        }
        1
    }

    /// Intercepted `PeekConsoleInputA` function.
    unsafe extern "system" fn hooked_peek_console_input_a(
        h_console_input: HANDLE,
        lp_buffer: *mut INPUT_RECORD,
        n_length: u32,
        lp_number_of_events_read: *mut u32,
    ) -> i32 {
        if n_length == 0 || lp_buffer.is_null() || lp_number_of_events_read.is_null() {
            return 0;
        }

        let queued = match QUEUED_INPUTS.lock() {
            Ok(q) => q,
            Err(e) => e.into_inner(),
        };

        if !queued.is_empty() {
            let mut count = 0;
            for (i, item) in queued.iter().take(n_length as usize).enumerate() {
                // SAFETY: i < n_length, so buffer index is in bounds.
                unsafe {
                    *lp_buffer.add(i) = *item;
                }
                count += 1;
            }
            // SAFETY: lp_number_of_events_read is non-null.
            unsafe {
                *lp_number_of_events_read = count;
            }
            return 1;
        }
        drop(queued);

        // SAFETY: Calling PeekConsoleInputW with provided arguments.
        unsafe {
            PeekConsoleInputW(
                h_console_input,
                lp_buffer,
                n_length,
                lp_number_of_events_read,
            )
        }
    }

    /// Install IAT hooks on `hlds.exe` (main module) and console control handler.
    pub fn install_hooks() {
        unsafe {
            // 1. Enforce UTF-8 console codepages
            SetConsoleCP(65001);
            SetConsoleOutputCP(65001);

            // 2. Install Console Ctrl Handler for graceful Ctrl+C shutdown
            SetConsoleCtrlHandler(Some(console_ctrl_handler), 1);

            // 3. Suppress tier0 assertion noise and dialogs
            suppress_tier0_spew();

            // 4. Locate main executable module (hlds.exe)
            let h_module = GetModuleHandleA(std::ptr::null());
            if h_module.is_null() {
                return;
            }
            let base = h_module as usize;

            // 5. Parse PE headers
            let dos_header = base as *const u8;
            if *dos_header != b'M' || *dos_header.add(1) != b'Z' {
                return;
            }
            let e_lfanew = *(dos_header.add(0x3c) as *const i32) as usize;
            let nt_headers = (base + e_lfanew) as *const u8;
            let pe_sig = *(nt_headers as *const u32);
            if pe_sig != 0x0000_4550 {
                return;
            }

            // In 32-bit PE, OptionalHeader starts at nt_headers + 24.
            // DataDirectory[1] (Import Directory) is at OptionalHeader + 96 + 8.
            let import_dir_ptr = (base + e_lfanew + 24 + 96 + 8) as *const u32;
            let import_rva = *import_dir_ptr as usize;
            let import_size = *import_dir_ptr.add(1) as usize;
            if import_rva == 0 || import_size == 0 {
                return;
            }

            let mut cur_desc = (base + import_rva) as *const u32;
            while *cur_desc != 0 || *cur_desc.add(4) != 0 {
                let original_first_thunk = *cur_desc as usize;
                let _name_rva = *cur_desc.add(3) as usize;
                let first_thunk = *cur_desc.add(4) as usize;

                let thunk_rva = if original_first_thunk != 0 {
                    original_first_thunk
                } else {
                    first_thunk
                };
                let iat_rva = first_thunk;

                let mut idx = 0;
                loop {
                    let thunk_entry = *((base + thunk_rva + idx * 4) as *const u32) as usize;
                    if thunk_entry == 0 {
                        break;
                    }

                    // Check if import by name (not ordinal)
                    if (thunk_entry & 0x8000_0000) == 0 {
                        let name_ptr = (base + thunk_entry + 2) as *const u8;
                        let fn_name =
                            std::ffi::CStr::from_ptr(name_ptr as *const std::os::raw::c_char)
                                .to_str()
                                .unwrap_or("");

                        let iat_entry_ptr = (base + iat_rva + idx * 4) as *mut usize;

                        if fn_name == "ReadConsoleInputA" {
                            let mut old_protect = 0;
                            if VirtualProtect(
                                iat_entry_ptr as *mut c_void,
                                std::mem::size_of::<usize>(),
                                PAGE_READWRITE,
                                &mut old_protect,
                            ) != 0
                            {
                                ORIG_READ_CONSOLE_INPUT_A =
                                    Some(std::mem::transmute::<usize, FnReadConsoleInputA>(
                                        *iat_entry_ptr,
                                    ));
                                *iat_entry_ptr = hooked_read_console_input_a as *const () as usize;
                                VirtualProtect(
                                    iat_entry_ptr as *mut c_void,
                                    std::mem::size_of::<usize>(),
                                    old_protect,
                                    &mut old_protect,
                                );
                                FlushInstructionCache(
                                    GetCurrentProcess(),
                                    iat_entry_ptr as *const c_void,
                                    std::mem::size_of::<usize>(),
                                );
                            }
                        } else if fn_name == "PeekConsoleInputA" {
                            let mut old_protect = 0;
                            if VirtualProtect(
                                iat_entry_ptr as *mut c_void,
                                std::mem::size_of::<usize>(),
                                PAGE_READWRITE,
                                &mut old_protect,
                            ) != 0
                            {
                                ORIG_PEEK_CONSOLE_INPUT_A =
                                    Some(std::mem::transmute::<usize, FnPeekConsoleInputA>(
                                        *iat_entry_ptr,
                                    ));
                                *iat_entry_ptr = hooked_peek_console_input_a as *const () as usize;
                                VirtualProtect(
                                    iat_entry_ptr as *mut c_void,
                                    std::mem::size_of::<usize>(),
                                    old_protect,
                                    &mut old_protect,
                                );
                                FlushInstructionCache(
                                    GetCurrentProcess(),
                                    iat_entry_ptr as *const c_void,
                                    std::mem::size_of::<usize>(),
                                );
                            }
                        } else if fn_name == "isprint" {
                            let mut old_protect = 0;
                            if VirtualProtect(
                                iat_entry_ptr as *mut c_void,
                                std::mem::size_of::<usize>(),
                                PAGE_READWRITE,
                                &mut old_protect,
                            ) != 0
                            {
                                ORIG_ISPRINT =
                                    Some(std::mem::transmute::<usize, FnIsPrint>(*iat_entry_ptr));
                                *iat_entry_ptr = hooked_isprint as *const () as usize;
                                VirtualProtect(
                                    iat_entry_ptr as *mut c_void,
                                    std::mem::size_of::<usize>(),
                                    old_protect,
                                    &mut old_protect,
                                );
                                FlushInstructionCache(
                                    GetCurrentProcess(),
                                    iat_entry_ptr as *const c_void,
                                    std::mem::size_of::<usize>(),
                                );
                            }
                        }
                    }

                    idx += 1;
                }

                cur_desc = cur_desc.add(5); // 20 bytes per IMAGE_IMPORT_DESCRIPTOR (5 * 4 bytes)
            }
        }
    }
}

/// Safe entry point to install console hooks and control handler on Windows.
pub fn init() {
    #[cfg(windows)]
    {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            imp::install_hooks();
        });
    }
}

/// Register engine server command function for graceful Ctrl+C shutdown.
pub fn register_shutdown_handler(cmd_fn: unsafe extern "C" fn(*const std::os::raw::c_char)) {
    #[cfg(windows)]
    {
        imp::register_shutdown_handler(cmd_fn);
    }
    #[cfg(not(windows))]
    {
        let _ = cmd_fn;
    }
}

/// Suppress Valve tier0 assertions and worker thread teardown warnings.
pub fn suppress_tier0_spew() {
    #[cfg(windows)]
    {
        imp::suppress_tier0_spew();
    }
}
