//! Engine network message dispatching, buffers, and user message registry.

use super::EngineBackend;
use crate::{call_engfunc, call_engfunc_ret};
use goldsrc_spi::engine::EngineMessages;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, RwLock};

static USER_MSG_REGISTRY: LazyLock<RwLock<HashMap<String, i32>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

pub type UserMsgResolverFn = fn(&str) -> i32;

static USER_MSG_RESOLVER_FN: std::sync::OnceLock<UserMsgResolverFn> = std::sync::OnceLock::new();

/// Sets a backend-specific resolver for finding user message IDs.
pub fn set_user_msg_resolver(resolver: UserMsgResolverFn) {
    let _ = USER_MSG_RESOLVER_FN.set(resolver);
}

/// Registers a known user message ID into the runtime registry.
pub fn register_user_msg_id(name: &str, id: i32) {
    if id > 0
        && id != 255
        && let Ok(mut map) = USER_MSG_REGISTRY.write()
    {
        map.insert(name.to_string(), id);
    }
}

static ACTIVE_MSG_TYPE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
static ACTIVE_MSG_DEST: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
static ACTIVE_MSG_RECEIVER: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
static ACTIVE_MSG_STRINGS: LazyLock<Mutex<Vec<String>>> = LazyLock::new(|| Mutex::new(Vec::new()));
static ACTIVE_MSG_BYTES: LazyLock<Mutex<Vec<u8>>> = LazyLock::new(|| Mutex::new(Vec::new()));

impl EngineMessages for EngineBackend {
    fn reg_user_msg(&self, name: &str, size: i32) -> i32 {
        if let Ok(map) = USER_MSG_REGISTRY.read()
            && let Some(&id) = map.get(name)
            && id > 0
            && id != 255
        {
            return id;
        }

        if let Some(resolver) = USER_MSG_RESOLVER_FN.get() {
            let id = resolver(name);
            if id > 0 && id != 255 {
                register_user_msg_id(name, id);
                return id;
            }
        }

        let engine_id = unsafe {
            if let Ok(cname) = std::ffi::CString::new(name) {
                call_engfunc_ret!((self.engfuncs)().pfnRegUserMsg, cname.as_ptr(), size)
            } else {
                0
            }
        };

        if engine_id > 0 && engine_id != 255 {
            register_user_msg_id(name, engine_id);
            return engine_id;
        }

        0
    }

    fn message_begin(
        &self,
        msg_dest: i32,
        msg_type: i32,
        origin: Option<[f32; 3]>,
        edict_index: Option<i32>,
    ) {
        ACTIVE_MSG_TYPE.store(msg_type, std::sync::atomic::Ordering::Relaxed);
        ACTIVE_MSG_DEST.store(msg_dest, std::sync::atomic::Ordering::Relaxed);
        ACTIVE_MSG_RECEIVER.store(
            edict_index.unwrap_or(0),
            std::sync::atomic::Ordering::Relaxed,
        );
        if let Ok(mut list) = ACTIVE_MSG_STRINGS.lock() {
            list.clear();
        }
        if let Ok(mut list) = ACTIVE_MSG_BYTES.lock() {
            list.clear();
        }

        unsafe {
            let porigin = match origin {
                Some(ref pos) => pos.as_ptr(),
                None => std::ptr::null(),
            };
            let pedict = match edict_index {
                Some(idx) => (self.engfuncs)()
                    .pfnPEntityOfEntIndex
                    .map(|f| f(idx))
                    .unwrap_or(std::ptr::null_mut()),
                None => std::ptr::null_mut(),
            };
            call_engfunc!(
                (self.engfuncs)().pfnMessageBegin,
                msg_dest,
                msg_type,
                porigin,
                pedict
            );
        }
    }

    fn message_end(&self) {
        let msg_type = ACTIVE_MSG_TYPE.swap(0, std::sync::atomic::Ordering::Relaxed);
        let _msg_dest = ACTIVE_MSG_DEST.swap(0, std::sync::atomic::Ordering::Relaxed);
        let _receiver = ACTIVE_MSG_RECEIVER.swap(0, std::sync::atomic::Ordering::Relaxed);
        if let Ok(mut list) = ACTIVE_MSG_STRINGS.lock() {
            list.clear();
        }
        let payload = if let Ok(mut list) = ACTIVE_MSG_BYTES.lock() {
            std::mem::take(&mut *list)
        } else {
            Vec::new()
        };

        if msg_type > 0 {
            let msg_name = if let Ok(map) = USER_MSG_REGISTRY.read() {
                map.iter()
                    .find(|(_, id)| **id == msg_type)
                    .map(|(k, _)| k.clone())
            } else {
                None
            };
            if let Some(name) = msg_name {
                let event_name = format!("user_msg_{}", name.to_ascii_lowercase());
                crate::hooks::dispatcher::emit(crate::host::HostEvent::Custom {
                    name: &event_name,
                    payload: &payload,
                });
            }
        }

        unsafe {
            call_engfunc!((self.engfuncs)().pfnMessageEnd);
        }
    }

    fn write_byte(&self, val: i32) {
        if ACTIVE_MSG_TYPE.load(std::sync::atomic::Ordering::Relaxed) > 0
            && let Ok(mut list) = ACTIVE_MSG_BYTES.lock()
        {
            list.push(val as u8);
        }
        unsafe {
            call_engfunc!((self.engfuncs)().pfnWriteByte, val);
        }
    }

    fn write_char(&self, val: i32) {
        if ACTIVE_MSG_TYPE.load(std::sync::atomic::Ordering::Relaxed) > 0
            && let Ok(mut list) = ACTIVE_MSG_BYTES.lock()
        {
            list.push(val as u8);
        }
        unsafe {
            call_engfunc!((self.engfuncs)().pfnWriteChar, val);
        }
    }

    fn write_short(&self, val: i32) {
        if ACTIVE_MSG_TYPE.load(std::sync::atomic::Ordering::Relaxed) > 0
            && let Ok(mut list) = ACTIVE_MSG_BYTES.lock()
        {
            list.extend_from_slice(&(val as i16).to_le_bytes());
        }
        unsafe {
            call_engfunc!((self.engfuncs)().pfnWriteShort, val);
        }
    }

    fn write_long(&self, val: i32) {
        if ACTIVE_MSG_TYPE.load(std::sync::atomic::Ordering::Relaxed) > 0
            && let Ok(mut list) = ACTIVE_MSG_BYTES.lock()
        {
            list.extend_from_slice(&val.to_le_bytes());
        }
        unsafe {
            call_engfunc!((self.engfuncs)().pfnWriteLong, val);
        }
    }

    fn write_angle(&self, val: f32) {
        if ACTIVE_MSG_TYPE.load(std::sync::atomic::Ordering::Relaxed) > 0
            && let Ok(mut list) = ACTIVE_MSG_BYTES.lock()
        {
            list.extend_from_slice(&val.to_le_bytes());
        }
        unsafe {
            call_engfunc!((self.engfuncs)().pfnWriteAngle, val);
        }
    }

    fn write_coord(&self, val: f32) {
        if ACTIVE_MSG_TYPE.load(std::sync::atomic::Ordering::Relaxed) > 0
            && let Ok(mut list) = ACTIVE_MSG_BYTES.lock()
        {
            list.extend_from_slice(&val.to_le_bytes());
        }
        unsafe {
            call_engfunc!((self.engfuncs)().pfnWriteCoord, val);
        }
    }

    fn write_string(&self, val: &str) {
        if ACTIVE_MSG_TYPE.load(std::sync::atomic::Ordering::Relaxed) > 0 {
            if let Ok(mut list) = ACTIVE_MSG_STRINGS.lock() {
                list.push(val.to_string());
            }
            if let Ok(mut list) = ACTIVE_MSG_BYTES.lock() {
                list.extend_from_slice(val.as_bytes());
                list.push(0);
            }
        }

        unsafe {
            let clean = val.replace('\0', "");
            let safe = if clean.len() > 500 {
                let mut end = 500;
                while end > 0 && !clean.is_char_boundary(end) {
                    end -= 1;
                }
                &clean[..end]
            } else {
                &clean
            };
            if let Ok(cstr) = std::ffi::CString::new(safe) {
                call_engfunc!((self.engfuncs)().pfnWriteString, cstr.as_ptr());
            }
        }
    }

    fn write_entity(&self, val: i32) {
        if ACTIVE_MSG_TYPE.load(std::sync::atomic::Ordering::Relaxed) > 0
            && let Ok(mut list) = ACTIVE_MSG_BYTES.lock()
        {
            list.extend_from_slice(&val.to_le_bytes());
        }
        unsafe {
            call_engfunc!((self.engfuncs)().pfnWriteEntity, val);
        }
    }
}
