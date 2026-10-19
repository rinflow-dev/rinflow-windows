//! Small Win32 helpers shared by the agent modules.

use std::ffi::c_void;
use std::io;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows_sys::Win32::System::Com::CoCreateGuid;
use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows_sys::Win32::System::Threading::{CreateMutexW, GetCurrentProcess, OpenProcessToken};
use windows_sys::core::GUID;

/// Null-terminated UTF-16 copy of `text` for `PCWSTR` parameters.
pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// Reads a null-terminated UTF-16 string owned by the system.
///
/// # Safety
/// `pointer` must be null or point to a null-terminated UTF-16 string.
pub unsafe fn from_wide(pointer: *const u16) -> String {
    if pointer.is_null() {
        return String::new();
    }
    let mut length = 0;
    // SAFETY: the caller guarantees null termination.
    while unsafe { *pointer.add(length) } != 0 {
        length += 1;
    }
    // SAFETY: `length` elements were just read successfully.
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(pointer, length) })
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: the handle was returned by a successful Win32 call.
        unsafe { CloseHandle(self.0) };
    }
}

/// Holds a named mutex for the life of the process to keep one agent per session.
pub struct SingleInstance(#[allow(dead_code)] OwnedHandle);

impl SingleInstance {
    pub fn acquire(name: &str) -> Option<Self> {
        let name = wide(name);
        // SAFETY: `name` is null-terminated and outlives the call.
        let handle = unsafe { CreateMutexW(null(), 0, name.as_ptr()) };
        if handle.is_null() {
            return None;
        }
        let handle = OwnedHandle(handle);
        // SAFETY: reads the calling thread's last-error value.
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            return None;
        }
        Some(Self(handle))
    }
}

pub fn current_user_sid() -> io::Result<String> {
    let mut token: HANDLE = null_mut();
    // SAFETY: the pseudo-handle from GetCurrentProcess needs no cleanup.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = OwnedHandle(token);

    let mut length = 0u32;
    // SAFETY: a null buffer queries the required size.
    unsafe { GetTokenInformation(token.0, TokenUser, null_mut(), 0, &mut length) };
    // u64 storage keeps TOKEN_USER (which starts with a pointer) aligned.
    let mut buffer = vec![0u64; (length as usize).div_ceil(8)];
    // SAFETY: `buffer` holds at least `length` bytes.
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            length,
            &mut length,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: GetTokenInformation filled the buffer with a TOKEN_USER.
    let user = unsafe { &*(buffer.as_ptr() as *const TOKEN_USER) };

    let mut sid_string = null_mut();
    // SAFETY: the SID points into `buffer`, which is still alive.
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut sid_string) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the system returned a null-terminated string we must LocalFree.
    let sid = unsafe { from_wide(sid_string) };
    unsafe { LocalFree(sid_string.cast()) };
    Ok(sid)
}

/// Whether the user picked the light app theme in Settings > Personalization.
/// Defaults to light, as Windows does, if the value is missing.
pub fn apps_use_light_theme() -> bool {
    let key = wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
    let value = wide("AppsUseLightTheme");
    let mut data = 1u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: `data` is a DWORD-sized buffer and `size` says so.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            null_mut(),
            (&mut data as *mut u32).cast(),
            &mut size,
        )
    };
    status != 0 || data != 0
}

/// Random GUID as a lowercase hyphenated string, used as the device ID.
pub fn new_guid_string() -> io::Result<String> {
    let mut guid = GUID {
        data1: 0,
        data2: 0,
        data3: 0,
        data4: [0; 8],
    };
    // SAFETY: CoCreateGuid only writes to the provided GUID.
    let result = unsafe { CoCreateGuid(&mut guid) };
    if result < 0 {
        return Err(io::Error::other(format!(
            "CoCreateGuid failed: {result:#010x}"
        )));
    }
    let d = guid.data4;
    Ok(format!(
        "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        guid.data1, guid.data2, guid.data3, d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]
    ))
}

/// Security attributes that let only the given user and LocalSystem open the
/// pipe. LocalSystem is kept for the future service that relaunches the agent.
pub struct PipeSecurity {
    attributes: SECURITY_ATTRIBUTES,
    descriptor: PSECURITY_DESCRIPTOR,
}

impl PipeSecurity {
    pub fn for_user(user_sid: &str) -> io::Result<Self> {
        let sddl = wide(&format!("D:P(A;;GA;;;{user_sid})(A;;GA;;;SY)"));
        let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
        // SAFETY: `sddl` is null-terminated; the descriptor is freed in Drop.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            attributes: SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor,
                bInheritHandle: 0,
            },
            descriptor,
        })
    }

    pub fn as_mut_ptr(&mut self) -> *mut c_void {
        (&mut self.attributes as *mut SECURITY_ATTRIBUTES).cast()
    }
}

impl Drop for PipeSecurity {
    fn drop(&mut self) {
        // SAFETY: the descriptor was allocated by the conversion call above.
        unsafe { LocalFree(self.descriptor) };
    }
}
