//! Engine console print and command execution operations.

use super::EngineBackend;
use crate::backend::print_queue::{sanitize_client_print, sanitize_server_print};
use crate::call_engfunc;
use goldsrc_spi::engine::EngineConsole;

impl EngineConsole for EngineBackend {
    fn server_print(&self, message: &str) {
        unsafe {
            let funcs = (self.engfuncs)();
            if let Some(f) = funcs.pfnServerPrint {
                for buffered in self.print_queue.drain() {
                    for line in buffered.lines() {
                        let bytes = sanitize_server_print(line);
                        f(bytes.as_ptr() as *const std::ffi::c_char);
                    }
                }
                for line in message.lines() {
                    let bytes = sanitize_server_print(line);
                    f(bytes.as_ptr() as *const std::ffi::c_char);
                }
            } else {
                self.print_queue.push(message);
            }
        }
    }

    fn client_print(&self, client_index: i32, print_type: i32, message: &str) {
        unsafe {
            let funcs = (self.engfuncs)();
            if let Some(pfn_p_entity_of_ent_index) = funcs.pfnPEntityOfEntIndex {
                let pedict = pfn_p_entity_of_ent_index(client_index);
                if !pedict.is_null() {
                    let safe_bytes = sanitize_client_print(message);
                    call_engfunc!(
                        funcs.pfnClientPrintf,
                        pedict,
                        print_type as _,
                        safe_bytes.as_ptr() as *const std::ffi::c_char
                    );
                }
            }
        }
    }

    fn server_command(&self, command: &str) {
        unsafe {
            let cmd = std::ffi::CString::new(command).unwrap_or_default();
            call_engfunc!((self.engfuncs)().pfnServerCommand, cmd.as_ptr());
        }
    }

    fn client_command(&self, client_index: i32, command: &str) {
        unsafe {
            let funcs = (self.engfuncs)();
            if let Some(pfn_p_entity_of_ent_index) = funcs.pfnPEntityOfEntIndex {
                let pedict = pfn_p_entity_of_ent_index(client_index);
                if !pedict.is_null() {
                    let cmd = std::ffi::CString::new(command).unwrap_or_default();
                    call_engfunc!(funcs.pfnClientCommand, pedict, cmd.as_ptr());
                }
            }
        }
    }
}
