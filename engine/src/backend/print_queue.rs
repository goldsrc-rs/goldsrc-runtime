//! Deferred server-print queue and safe formatting utilities.

use std::collections::VecDeque;
use std::sync::Mutex;

/// Deferred server-print queue.
///
/// Printing is deferred to the post-start-frame hook because the engine is
/// unstable if a plugin prints mid-frame. Also escapes fmtlib-sensitive
/// characters (`%`, `{`, `}`) — ReHLDS routes `ServerPrint` through fmtlib
/// and unescaped braces would crash the server.
pub struct PrintQueue(Mutex<VecDeque<String>>);

/// Helper to escape format specifiers and braces for ReHLDS fmtlib safety.
///
/// Strips NUL bytes, escapes `%` → `%%`, `{`/`}` → `{{`/`}}`, CR/LF stripped, lines trimmed to 1024 chars.
pub fn escape_server_print(message: &str) -> String {
    let safe = message
        .replace('\0', "")
        .replace('%', "%%")
        .replace('{', "{{")
        .replace('}', "}}")
        .replace('\r', "")
        .replace('\n', " ");
    let mut end = safe.len().min(1024);
    while end > 0 && !safe.is_char_boundary(end) {
        end -= 1;
    }
    let slice = &safe[..end];
    format!("{}\n", slice.trim_end_matches(['\r', '\n']))
}

/// Helper to sanitize server console prints with fmtlib escaping in UTF-8 encoding.
pub fn sanitize_server_print(line: &str) -> Vec<u8> {
    let safe = escape_server_print(line);
    let mut out = safe.into_bytes();
    out.push(0); // NUL terminator
    out
}

/// Helper to sanitize console/center/chat prints with CP1251 encoding for Cyrillic GoldSrc client support.
pub fn sanitize_client_print(message: &str) -> Vec<u8> {
    let cp1251_bytes = goldsrc_api::utf8_to_cp1251(message);
    let mut out = Vec::with_capacity(cp1251_bytes.len() + 1);
    for &b in &cp1251_bytes {
        if b == 0 {
            continue;
        }
        if b == b'%' {
            out.push(b'%');
            out.push(b'%');
        } else {
            out.push(b);
        }
    }
    out.push(0); // NUL terminator
    out
}

/// Converts an engine-provided C string pointer to an owned `String`
/// (empty on null). Supports both UTF-8 and CP1251 (Cyrillic) decoding.
///
/// # Safety
/// `ptr` must be null or point to readable memory.
pub unsafe fn cstr_to_string(ptr: *const std::ffi::c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    let len = unsafe { goldsrc_sys::ffi::libc_strnlen(ptr, 1024) };
    if len == 0 {
        return String::new();
    }
    let slice = unsafe { std::slice::from_raw_parts(ptr as *const u8, len) };
    if let Ok(s) = std::str::from_utf8(slice) {
        s.trim().to_string()
    } else {
        goldsrc_api::cp1251_to_utf8(slice).trim().to_string()
    }
}

impl Default for PrintQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl PrintQueue {
    /// Create an empty print queue.
    pub const fn new() -> Self {
        Self(Mutex::new(VecDeque::new()))
    }

    /// Add a message to the back of the queue.
    pub fn push(&self, message: &str) {
        let mut queue = match self.0.lock() {
            Ok(q) => q,
            Err(e) => e.into_inner(),
        };
        queue.push_back(message.to_string());
    }

    /// Take all pending messages, escaping fmtlib-sensitive characters.
    pub fn drain(&self) -> Vec<String> {
        let messages = {
            let mut queue = match self.0.lock() {
                Ok(q) => q,
                Err(e) => e.into_inner(),
            };
            if queue.is_empty() {
                return Vec::new();
            }
            std::mem::take(&mut *queue)
        };
        messages
            .into_iter()
            .map(|message| escape_server_print(&message))
            .collect()
    }
}
