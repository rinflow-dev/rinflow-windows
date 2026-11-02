// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Win32 helpers shared by the agent, the input helper and the service.

use std::ffi::c_void;
use std::io;
use std::path::{Path, PathBuf};
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
    SDDL_REVISION_1, SE_KERNEL_OBJECT,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
    SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_SET_VALUE, KEY_WOW64_64KEY, REG_DWORD, REG_OPTION_NON_VOLATILE,
    RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RRF_SUBKEY_WOW6464KEY, RegCloseKey, RegCreateKeyExW,
    RegGetValueW, RegSetValueExW,
};
use windows_sys::Win32::System::RemoteDesktop::{ProcessIdToSessionId, WTSQueryUserToken};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, OpenProcess, OpenProcessToken, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};

/// Null-terminated UTF-16 copy of `text` for `PCWSTR` parameters.
pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// Closes a handle when dropped.
pub struct Handle(pub HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: a handle returned by a successful Win32 call.
            unsafe { CloseHandle(self.0) };
        }
    }
}

// Kernel handles may be used from any thread.
unsafe impl Send for Handle {}

pub fn current_session() -> u32 {
    process_session(std::process::id()).unwrap_or(u32::MAX)
}

pub fn process_session(pid: u32) -> Option<u32> {
    let mut session = 0;
    // SAFETY: writes the session ID into `session`.
    (unsafe { ProcessIdToSessionId(pid, &mut session) } != 0).then_some(session)
}

/// The full path of a running process's executable.
pub fn process_image(pid: u32) -> Option<PathBuf> {
    // SAFETY: plain process query; the handle is closed by `Handle`.
    let process = Handle(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) });
    if process.0.is_null() {
        return None;
    }
    image_of(process.0)
}

fn image_of(process: HANDLE) -> Option<PathBuf> {
    let mut buffer = vec![0u16; 1024];
    let mut length = buffer.len() as u32;
    // SAFETY: `buffer` holds `length` UTF-16 units.
    if unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            buffer.as_mut_ptr(),
            &mut length,
        )
    } == 0
    {
        return None;
    }
    Some(PathBuf::from(String::from_utf16_lossy(
        &buffer[..length as usize],
    )))
}

/// This executable's folder, as the system names it.
pub fn own_dir() -> Option<PathBuf> {
    // SAFETY: the pseudo-handle of this process needs no cleanup.
    let image = image_of(unsafe { GetCurrentProcess() })?;
    image.parent().map(Path::to_path_buf)
}

/// This PC's Program Files folder, as administrators set it in HKLM; not the
/// `ProgramFiles` variable, which comes from whoever started this process.
fn program_files_dir() -> Option<PathBuf> {
    let key = wide(r"SOFTWARE\Microsoft\Windows\CurrentVersion");
    let value = wide("ProgramFilesDir");
    let mut buffer = vec![0u16; 1024];
    let mut size = (buffer.len() * size_of::<u16>()) as u32;
    // SAFETY: `buffer` holds `size` bytes.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ | RRF_SUBKEY_WOW6464KEY,
            null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if status != 0 {
        return None;
    }
    let length = buffer
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(buffer.len());
    (length > 0).then(|| PathBuf::from(String::from_utf16_lossy(&buffer[..length])))
}

/// Whether `dir` is in Program Files, where only administrators can change
/// files. The LocalSystem service trusts the programs beside it, so it runs
/// from nowhere else (a folder picked elsewhere usually lets every user
/// write to it).
pub fn in_program_files(dir: &Path) -> bool {
    let Some(root) = program_files_dir() else {
        return false;
    };
    let root = root.to_string_lossy().trim_end_matches('\\').to_lowercase();
    let dir = dir.to_string_lossy().to_lowercase();
    dir.strip_prefix(&root)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('\\'))
}

/// Whether the input service can be installed from this program's folder:
/// the setup program put this copy into Program Files, with the service
/// beside it. A portable copy (or a development build) cannot.
pub fn service_installable() -> bool {
    own_dir().is_some_and(|dir| in_program_files(&dir) && dir.join(crate::SERVICE_EXE).is_file())
}

/// Whether process `pid` runs `exe` from this executable's folder, in this
/// session: the other LinkCursor program, installed with this one.
pub fn is_sibling_process(pid: u32, exe: &str) -> bool {
    let (Some(dir), Some(image)) = (own_dir(), process_image(pid)) else {
        return false;
    };
    let expected = dir.join(exe);
    let same = image
        .to_string_lossy()
        .eq_ignore_ascii_case(&expected.to_string_lossy());
    same && process_session(pid) == Some(current_session())
}

/// SID string of the user signed in to `session`. As LocalSystem, an error
/// while nobody is signed in there (the sign-in screen). Not as LocalSystem
/// (a development run), the user running this process.
pub fn session_user_sid(session: u32) -> io::Result<String> {
    let mut token: HANDLE = null_mut();
    // SAFETY: needs SeTcbPrivilege (LocalSystem); the handle is closed below.
    let from_session = unsafe { WTSQueryUserToken(session, &mut token) } != 0;
    if !from_session {
        let error = io::Error::last_os_error();
        if is_local_system() {
            return Err(error);
        }
        // SAFETY: the pseudo-handle of this process needs no cleanup.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    let token = Handle(token);
    token_user_sid(token.0)
}

/// SID string of a kernel object's owner, read through `object`'s handle
/// (opened with `READ_CONTROL`).
pub fn object_owner_sid(object: &impl std::os::windows::io::AsRawHandle) -> io::Result<String> {
    let mut owner: PSID = null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: an open handle; `owner` points into `descriptor`, freed below.
    let status = unsafe {
        GetSecurityInfo(
            object.as_raw_handle(),
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            null_mut(),
            null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let sid = sid_string(owner);
    // SAFETY: allocated by GetSecurityInfo.
    unsafe { LocalFree(descriptor) };
    sid
}

/// LocalSystem, or the Administrators group (the default owner LocalSystem
/// gives its objects): only an elevated or system process can make either
/// the owner.
pub fn is_privileged_owner(sid: &str) -> bool {
    sid == "S-1-5-18" || sid == "S-1-5-32-544"
}

fn token_user_sid(token: HANDLE) -> io::Result<String> {
    let mut length = 0u32;
    // SAFETY: a null buffer queries the required size.
    unsafe { GetTokenInformation(token, TokenUser, null_mut(), 0, &mut length) };
    // u64 storage keeps TOKEN_USER (which starts with a pointer) aligned.
    let mut buffer = vec![0u64; (length as usize).div_ceil(8)];
    // SAFETY: `buffer` holds at least `length` bytes.
    if unsafe {
        GetTokenInformation(
            token,
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
    // The SID points into `buffer`, which is still alive.
    sid_string(user.User.Sid)
}

fn sid_string(sid: PSID) -> io::Result<String> {
    let mut text = null_mut();
    // SAFETY: the caller passes a valid SID.
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut length = 0;
    // SAFETY: the system returned a null-terminated string we must LocalFree.
    let sid = unsafe {
        while *text.add(length) != 0 {
            length += 1;
        }
        let sid = String::from_utf16_lossy(std::slice::from_raw_parts(text, length));
        LocalFree(text.cast());
        sid
    };
    Ok(sid)
}

/// Security attributes from an SDDL string.
pub struct SecurityAttributes {
    attributes: SECURITY_ATTRIBUTES,
    descriptor: PSECURITY_DESCRIPTOR,
}

impl SecurityAttributes {
    pub fn from_sddl(sddl: &str) -> io::Result<Self> {
        let sddl = wide(sddl);
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

impl Drop for SecurityAttributes {
    fn drop(&mut self) {
        // SAFETY: the descriptor was allocated by the conversion call above.
        unsafe { LocalFree(self.descriptor) };
    }
}

/// The machine-wide "elevated input" setting; `None` when not set (not
/// installed).
pub fn elevated_input_setting() -> Option<bool> {
    let key = wide(crate::REGISTRY_KEY);
    let value = wide(crate::ELEVATED_INPUT_VALUE);
    let mut data = 0u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: `data` is a DWORD-sized buffer and `size` says so.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD | RRF_SUBKEY_WOW6464KEY,
            null_mut(),
            (&mut data as *mut u32).cast(),
            &mut size,
        )
    };
    (status == 0).then_some(data != 0)
}

/// Writes the machine-wide setting; needs an administrator.
pub fn set_elevated_input_setting(enabled: bool) -> io::Result<()> {
    let key_name = wide(crate::REGISTRY_KEY);
    let value = wide(crate::ELEVATED_INPUT_VALUE);
    let mut key: HKEY = null_mut();
    // SAFETY: creates or opens the key; closed below.
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            key_name.as_ptr(),
            0,
            null_mut(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE | KEY_WOW64_64KEY,
            null_mut(),
            &mut key,
            null_mut(),
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let data = u32::from(enabled).to_le_bytes();
    // SAFETY: a DWORD value from a 4-byte buffer.
    let status = unsafe {
        let status = RegSetValueExW(key, value.as_ptr(), 0, REG_DWORD, data.as_ptr(), 4);
        RegCloseKey(key);
        status
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    Ok(())
}

/// Whether this process runs as LocalSystem.
pub fn is_local_system() -> bool {
    let mut token: HANDLE = null_mut();
    // SAFETY: the pseudo-handle of this process needs no cleanup.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return false;
    }
    let token = Handle(token);
    token_user_sid(token.0).is_ok_and(|sid| sid == "S-1-5-18")
}

pub fn current_pid() -> u32 {
    // SAFETY: no arguments.
    unsafe { GetCurrentProcessId() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_folders_inside_program_files_count() {
        let root = program_files_dir().expect("ProgramFilesDir is set on every Windows");
        assert!(in_program_files(&root.join("LinkCursor")));
        // Paths differ in case only: the same folder.
        let upper = PathBuf::from(root.to_string_lossy().to_uppercase());
        assert!(in_program_files(&upper.join("LinkCursor")));
        // A folder whose name only starts the same is elsewhere.
        let mut lookalike = root.clone().into_os_string();
        lookalike.push(" Evil");
        assert!(!in_program_files(
            &PathBuf::from(lookalike).join("LinkCursor")
        ));
        assert!(!in_program_files(Path::new(r"C:\LinkCursor")));
    }
}
