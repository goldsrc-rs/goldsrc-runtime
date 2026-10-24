//! Low-level C-ABI declarations and vtables for ReHLDS and ReGameDLL.
//!
//! Provides zero-overhead access to extended engine and game DLL interfaces
//! exposed via Valve's `CreateInterface` mechanism.

#![allow(non_snake_case, non_camel_case_types)]
use std::ffi::{c_char, c_double, c_float, c_int, c_uchar, c_uint, c_ushort, c_void};

pub use goldsrc_sys::{DLL_FUNCTIONS, cvar_t, edict_t, entvars_t, usercmd_s};

/// ReHLDS Engine API interface version string.
pub const VREHLDS_HLDS_API_VERSION: &[u8] = b"VREHLDS_HLDS_API_VERSION001\0";

/// ReGameDLL API interface version string.
pub const VRE_GAMEDLL_API_VERSION: &[u8] = b"VRE_GAMEDLL_API_VERSION001\0";

/// Major version expected for ReHLDS API.
pub const REHLDS_API_VERSION_MAJOR: c_int = 3;
/// Minimum minor version expected for ReHLDS API.
pub const REHLDS_API_VERSION_MINOR: c_int = 11;

/// Major version expected for ReGameDLL API.
pub const REGAMEDLL_API_VERSION_MAJOR: c_int = 5;
/// Minimum minor version expected for ReGameDLL API.
pub const REGAMEDLL_API_VERSION_MINOR: c_int = 3;

/// Interface factory function signature (`CreateInterface`).
pub type CreateInterfaceFn =
    unsafe extern "C" fn(pName: *const c_char, pReturnCode: *mut c_int) -> *mut c_void;

// ----------------------------------------------------------------------------
// ReHLDS C-ABI Types & VTables
// ----------------------------------------------------------------------------

/// ReHLDS Engine Function Table.
#[repr(C)]
pub struct RehldsFuncs_t {
    pub DropClient:
        Option<unsafe extern "C" fn(cl: *mut c_void, crash: bool, fmt: *const c_char, ...)>,
    pub RejectConnection: Option<unsafe extern "C" fn(adr: *mut c_void, fmt: *const c_char, ...)>,
    pub SteamNotifyBotConnect: Option<unsafe extern "C" fn(cl: *mut c_void) -> c_int>,
    pub GetNetMessage: Option<unsafe extern "C" fn() -> *mut c_void>,
    pub GetHostClient: Option<unsafe extern "C" fn() -> *mut c_void>,
    pub GetMsgReadCount: Option<unsafe extern "C" fn() -> *mut c_int>,
    pub FilterUser: Option<unsafe extern "C" fn(user: *mut c_void) -> c_int>,
    pub NET_SendPacket:
        Option<unsafe extern "C" fn(length: c_uint, data: *mut c_void, to: *const c_void)>,
    pub TokenizeString: Option<unsafe extern "C" fn(s: *mut c_char)>,
    pub CheckChallenge: Option<unsafe extern "C" fn(adr: *const c_void, challenge: c_int) -> bool>,
    pub SendUserReg: Option<unsafe extern "C" fn(msg: *mut c_void)>,
    pub WriteDeltaDescriptionsToClient: Option<unsafe extern "C" fn(msg: *mut c_void)>,
    pub SetMoveVars: Option<unsafe extern "C" fn()>,
    pub WriteMovevarsToClient: Option<unsafe extern "C" fn(msg: *mut c_void)>,
    pub GetClientFallback: Option<unsafe extern "C" fn() -> *mut c_char>,
    pub GetAllowCheats: Option<unsafe extern "C" fn() -> *mut c_int>,
    pub GSBSecure: Option<unsafe extern "C" fn() -> bool>,
    pub GetBuildNumber: Option<unsafe extern "C" fn() -> c_int>,
    pub GetRealTime: Option<unsafe extern "C" fn() -> c_double>,
    pub GetMsgBadRead: Option<unsafe extern "C" fn() -> *mut c_int>,
    pub GetCmdSource: Option<unsafe extern "C" fn() -> *mut c_int>,
    pub Log: Option<unsafe extern "C" fn(prefix: *const c_char, msg: *const c_char)>,
    pub GetEntityInterface: Option<unsafe extern "C" fn() -> *mut DLL_FUNCTIONS>,
    pub EV_PlayReliableEvent: Option<
        unsafe extern "C" fn(
            cl: *mut c_void,
            entindex: c_int,
            eventindex: c_ushort,
            delay: c_float,
            pargs: *mut c_void,
        ),
    >,
    pub SV_LookupSoundIndex: Option<unsafe extern "C" fn(sample: *const c_char) -> c_int>,
    pub MSG_StartBitWriting: Option<unsafe extern "C" fn(buf: *mut c_void)>,
    pub MSG_WriteBits: Option<unsafe extern "C" fn(data: u32, numbits: c_int)>,
    pub MSG_WriteBitVec3Coord: Option<unsafe extern "C" fn(fa: *const c_float)>,
    pub MSG_EndBitWriting: Option<unsafe extern "C" fn(buf: *mut c_void)>,
    pub SZ_GetSpace: Option<unsafe extern "C" fn(buf: *mut c_void, length: c_int) -> *mut c_void>,
    pub GetCvarVars: Option<unsafe extern "C" fn() -> *mut cvar_t>,
    pub SV_GetChallenge: Option<unsafe extern "C" fn(adr: *const c_void) -> c_int>,
    pub SV_AddResource: Option<
        unsafe extern "C" fn(
            r_type: c_int,
            name: *const c_char,
            size: c_int,
            flags: c_uchar,
            index: c_int,
        ),
    >,
    pub MSG_ReadShort: Option<unsafe extern "C" fn() -> c_int>,
    pub MSG_ReadBuf: Option<unsafe extern "C" fn(iSize: c_int, pbuf: *mut c_void) -> c_int>,
    pub MSG_WriteBuf: Option<unsafe extern "C" fn(sb: *mut c_void, iSize: c_int, buf: *mut c_void)>,
    pub MSG_WriteByte: Option<unsafe extern "C" fn(sb: *mut c_void, c: c_int)>,
    pub MSG_WriteShort: Option<unsafe extern "C" fn(sb: *mut c_void, c: c_int)>,
    pub MSG_WriteString: Option<unsafe extern "C" fn(sb: *mut c_void, s: *const c_char)>,
    pub GetPluginApi: Option<unsafe extern "C" fn(name: *const c_char) -> *mut c_void>,
    pub RegisterPluginApi: Option<unsafe extern "C" fn(name: *const c_char, impl_ptr: *mut c_void)>,
    pub SV_FileInConsistencyList:
        Option<unsafe extern "C" fn(filename: *const c_char, ppconsist: *mut *mut c_void) -> c_int>,
    pub Steam_NotifyClientConnect: Option<
        unsafe extern "C" fn(
            cl: *mut c_void,
            pvSteam2Key: *const c_void,
            ucbSteam2Key: c_uint,
        ) -> c_int,
    >,
    pub Steam_NotifyClientDisconnect: Option<unsafe extern "C" fn(cl: *mut c_void)>,
    pub SV_StartSound: Option<
        unsafe extern "C" fn(
            recipients: c_int,
            entity: *mut edict_t,
            channel: c_int,
            sample: *const c_char,
            volume: c_int,
            attenuation: c_float,
            flags: c_int,
            pitch: c_int,
        ),
    >,
    pub SV_EmitSound2: Option<
        unsafe extern "C" fn(
            entity: *mut edict_t,
            receiver: *mut c_void,
            channel: c_int,
            sample: *const c_char,
            volume: c_float,
            attenuation: c_float,
            flags: c_int,
            pitch: c_int,
            emitFlags: c_int,
            pOrigin: *const c_float,
        ) -> bool,
    >,
    pub SV_UpdateUserInfo: Option<unsafe extern "C" fn(pGameClient: *mut c_void)>,
    pub StripUnprintableAndSpace: Option<unsafe extern "C" fn(pch: *mut c_char) -> bool>,
    pub Cmd_RemoveCmd: Option<unsafe extern "C" fn(cmd_name: *const c_char)>,
    pub GetCommandMatches:
        Option<unsafe extern "C" fn(string: *const c_char, pMatchList: *mut c_void)>,
    pub AddExtDll: Option<unsafe extern "C" fn(hModule: *mut c_void) -> bool>,
    pub AddCvarListener: Option<unsafe extern "C" fn(var_name: *const c_char, func: *mut c_void)>,
    pub RemoveExtDll: Option<unsafe extern "C" fn(hModule: *mut c_void)>,
    pub RemoveCvarListener:
        Option<unsafe extern "C" fn(var_name: *const c_char, func: *mut c_void)>,
    pub GetEntityInit: Option<unsafe extern "C" fn(pszClassName: *mut c_char) -> *mut c_void>,
    pub MSG_ReadChar: Option<unsafe extern "C" fn() -> c_int>,
    pub MSG_ReadByte: Option<unsafe extern "C" fn() -> c_int>,
    pub MSG_ReadLong: Option<unsafe extern "C" fn() -> c_int>,
    pub MSG_ReadFloat: Option<unsafe extern "C" fn() -> c_float>,
    pub MSG_ReadString: Option<unsafe extern "C" fn() -> *mut c_char>,
    pub MSG_ReadStringLine: Option<unsafe extern "C" fn() -> *mut c_char>,
    pub MSG_ReadAngle: Option<unsafe extern "C" fn() -> c_float>,
    pub MSG_ReadHiresAngle: Option<unsafe extern "C" fn() -> c_float>,
    pub MSG_ReadUsercmd: Option<unsafe extern "C" fn(to: *mut usercmd_s, from: *mut usercmd_s)>,
    pub MSG_ReadCoord: Option<unsafe extern "C" fn() -> c_float>,
    pub MSG_ReadVec3Coord: Option<unsafe extern "C" fn(sb: *mut c_void, fa: *mut c_float)>,
}

/// Main ReHLDS API interface VTable representation.
#[repr(C)]
pub struct IRehldsApiVtbl {
    pub destructor: unsafe extern "C" fn(this: *mut c_void),
    pub GetMajorVersion: unsafe extern "C" fn(this: *mut c_void) -> c_int,
    pub GetMinorVersion: unsafe extern "C" fn(this: *mut c_void) -> c_int,
    pub GetFuncs: unsafe extern "C" fn(this: *mut c_void) -> *const RehldsFuncs_t,
    pub GetHookchains: unsafe extern "C" fn(this: *mut c_void) -> *mut c_void,
    pub GetServerStatic: unsafe extern "C" fn(this: *mut c_void) -> *mut c_void,
    pub GetServerData: unsafe extern "C" fn(this: *mut c_void) -> *mut c_void,
    pub GetFlightRecorder: unsafe extern "C" fn(this: *mut c_void) -> *mut c_void,
    pub GetMem: unsafe extern "C" fn(this: *mut c_void) -> *mut c_void,
}

/// ReHLDS API Interface pointer wrapper.
#[repr(C)]
pub struct IRehldsApi {
    pub vtable: *const IRehldsApiVtbl,
}

// ----------------------------------------------------------------------------
// ReGameDLL C-ABI Types & VTables
// ----------------------------------------------------------------------------

/// ReGameDLL Global Function Table.
#[repr(C)]
pub struct ReGameFuncs_t {
    pub CREATE_NAMED_ENTITY2: Option<unsafe extern "C" fn(iClass: c_uint) -> *mut edict_t>,
    pub ChangeString: Option<unsafe extern "C" fn(dest: *mut *mut c_char, source: *const c_char)>,
    pub RadiusDamage: Option<
        unsafe extern "C" fn(
            vecSrc: [c_float; 3],
            pevInflictor: *mut entvars_t,
            pevAttacker: *mut entvars_t,
            flDamage: c_float,
            flRadius: c_float,
            iClassIgnore: c_int,
            bitsDamageType: c_int,
        ),
    >,
    pub ClearMultiDamage: Option<unsafe extern "C" fn()>,
    pub ApplyMultiDamage:
        Option<unsafe extern "C" fn(pevInflictor: *mut entvars_t, pevAttacker: *mut entvars_t)>,
    pub AddMultiDamage: Option<
        unsafe extern "C" fn(
            pevInflictor: *mut entvars_t,
            pEntity: *mut c_void,
            flDamage: c_float,
            bitsDamageType: c_int,
        ),
    >,
    pub UTIL_FindEntityByString: Option<
        unsafe extern "C" fn(
            pStartEntity: *mut c_void,
            szKeyword: *const c_char,
            szValue: *const c_char,
        ) -> *mut c_void,
    >,
    pub AddEntityHashValue:
        Option<unsafe extern "C" fn(pev: *mut entvars_t, value: *const c_char, fieldType: c_int)>,
    pub RemoveEntityHashValue:
        Option<unsafe extern "C" fn(pev: *mut entvars_t, value: *const c_char, fieldType: c_int)>,
    pub Cmd_Argc: Option<unsafe extern "C" fn() -> c_int>,
    pub Cmd_Argv: Option<unsafe extern "C" fn(i: c_int) -> *const c_char>,
}

/// ReGameDLL API interface VTable representation.
#[repr(C)]
pub struct IReGameApiVtbl {
    pub destructor: unsafe extern "C" fn(this: *mut c_void),
    pub GetMajorVersion: unsafe extern "C" fn(this: *mut c_void) -> c_int,
    pub GetMinorVersion: unsafe extern "C" fn(this: *mut c_void) -> c_int,
    pub GetFuncs: unsafe extern "C" fn(this: *mut c_void) -> *const ReGameFuncs_t,
    pub GetHookchains: unsafe extern "C" fn(this: *mut c_void) -> *mut c_void,
    pub GetGameRules: unsafe extern "C" fn(this: *mut c_void) -> *mut c_void,
    pub GetWeaponInfo_Id: unsafe extern "C" fn(this: *mut c_void, weaponID: c_int) -> *mut c_void,
    pub GetWeaponInfo_Name:
        unsafe extern "C" fn(this: *mut c_void, weaponName: *const c_char) -> *mut c_void,
    pub GetPlayerMove: unsafe extern "C" fn(this: *mut c_void) -> *mut c_void,
    pub GetWeaponSlot_Id: unsafe extern "C" fn(this: *mut c_void, weaponID: c_int) -> *mut c_void,
    pub GetWeaponSlot_Name:
        unsafe extern "C" fn(this: *mut c_void, weaponName: *const c_char) -> *mut c_void,
    pub GetItemInfo: unsafe extern "C" fn(this: *mut c_void, weaponID: c_int) -> *mut c_void,
    pub GetAmmoInfo: unsafe extern "C" fn(this: *mut c_void, ammoID: c_int) -> *mut c_void,
}

/// ReGameDLL API Interface pointer wrapper.
#[repr(C)]
pub struct IReGameApi {
    pub vtable: *const IReGameApiVtbl,
}
