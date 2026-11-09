// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! HTTP GET through WinHTTP: Windows' own TLS and certificate store, and the
//! system's proxy settings (including automatic configuration), so updates
//! work wherever the browser does. Redirects are followed (GitHub sends
//! release downloads on to its content servers).

use crate::win::wide;
use std::io;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Networking::WinHttp::{
    URL_COMPONENTS, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE,
    WINHTTP_INTERNET_SCHEME_HTTP, WINHTTP_INTERNET_SCHEME_HTTPS, WINHTTP_QUERY_FLAG_NUMBER,
    WINHTTP_QUERY_STATUS_CODE, WinHttpCloseHandle, WinHttpConnect, WinHttpCrackUrl, WinHttpOpen,
    WinHttpOpenRequest, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse,
    WinHttpSendRequest, WinHttpSetTimeouts,
};

/// A WinHTTP handle, closed when dropped.
struct Handle(*mut core::ffi::c_void);

impl Handle {
    fn new(handle: *mut core::ffi::c_void) -> io::Result<Self> {
        if handle.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(handle))
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: a handle WinHTTP returned, closed once.
        unsafe {
            WinHttpCloseHandle(self.0);
        }
    }
}

/// Where a URL points.
struct Target {
    secure: bool,
    host: Vec<u16>,
    port: u16,
    /// Path and query, NUL-terminated.
    object: Vec<u16>,
}

fn crack(url: &str) -> io::Result<Target> {
    let url = wide(url);
    let mut parts = URL_COMPONENTS {
        dwStructSize: size_of::<URL_COMPONENTS>() as u32,
        // u32::MAX: point into `url` instead of copying.
        dwSchemeLength: u32::MAX,
        dwHostNameLength: u32::MAX,
        dwUrlPathLength: u32::MAX,
        dwExtraInfoLength: u32::MAX,
        ..Default::default()
    };
    // SAFETY: `url` is NUL-terminated and outlives `parts`, whose pointers
    // point into it.
    unsafe {
        if WinHttpCrackUrl(url.as_ptr(), 0, 0, &mut parts) == 0 {
            return Err(io::Error::last_os_error());
        }
        let slice = |pointer: *mut u16, length: u32| {
            if pointer.is_null() {
                &[][..]
            } else {
                std::slice::from_raw_parts(pointer, length as usize)
            }
        };
        let host = slice(parts.lpszHostName, parts.dwHostNameLength).to_vec();
        let secure = match parts.nScheme {
            WINHTTP_INTERNET_SCHEME_HTTPS => true,
            // Plain HTTP only to this PC (a test feed): the contents are
            // signed either way, but nothing else should travel unencrypted.
            WINHTTP_INTERNET_SCHEME_HTTP
                if matches!(
                    String::from_utf16_lossy(&host).as_str(),
                    "localhost" | "127.0.0.1"
                ) =>
            {
                false
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "only https addresses are used",
                ));
            }
        };
        let mut object = slice(parts.lpszUrlPath, parts.dwUrlPathLength).to_vec();
        object.extend_from_slice(slice(parts.lpszExtraInfo, parts.dwExtraInfoLength));
        if object.is_empty() {
            object.push(u16::from(b'/'));
        }
        object.push(0);
        let mut host = host;
        host.push(0);
        Ok(Target {
            secure,
            host,
            port: parts.nPort,
            object,
        })
    }
}

/// Fetches `url`, handing the body to `sink` piece by piece with the total
/// length when the server gave it. Fails on any status but 200, and once
/// the body grows past `limit` bytes.
pub fn get(
    url: &str,
    limit: u64,
    mut sink: impl FnMut(&[u8], Option<u64>) -> io::Result<()>,
) -> io::Result<()> {
    let target = crack(url)?;
    let agent = wide(concat!("Rinflow/", env!("CARGO_PKG_VERSION")));
    let get = wide("GET");
    // SAFETY: every string is NUL-terminated and outlives the call it is
    // passed to; handles are closed by `Handle` in reverse order.
    unsafe {
        let session = Handle::new(WinHttpOpen(
            agent.as_ptr(),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            null(),
            null(),
            0,
        ))?;
        // Name lookup, connecting, sending: 30 s each; a stalled download
        // gives up after a minute without data.
        WinHttpSetTimeouts(session.0, 30_000, 30_000, 30_000, 60_000);
        let connection = Handle::new(WinHttpConnect(
            session.0,
            target.host.as_ptr(),
            target.port,
            0,
        ))?;
        let request = Handle::new(WinHttpOpenRequest(
            connection.0,
            get.as_ptr(),
            target.object.as_ptr(),
            null(),
            null(),
            null(),
            if target.secure {
                WINHTTP_FLAG_SECURE
            } else {
                0
            },
        ))?;
        if WinHttpSendRequest(request.0, null(), 0, null(), 0, 0, 0) == 0
            || WinHttpReceiveResponse(request.0, null_mut()) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut status = 0u32;
        let mut size = size_of::<u32>() as u32;
        if WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            null(),
            (&mut status as *mut u32).cast(),
            &mut size,
            null_mut(),
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        if status != 200 {
            return Err(io::Error::other(format!("the server answered {status}")));
        }
        let total = content_length(&request);
        if total.is_some_and(|total| total > limit) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the download is larger than expected",
            ));
        }
        let mut buffer = vec![0u8; 64 * 1024];
        let mut received = 0u64;
        loop {
            let mut read = 0u32;
            if WinHttpReadData(
                request.0,
                buffer.as_mut_ptr().cast(),
                buffer.len() as u32,
                &mut read,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            if read == 0 {
                break;
            }
            received += u64::from(read);
            if received > limit {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "the download is larger than expected",
                ));
            }
            sink(&buffer[..read as usize], total)?;
        }
        if total.is_some_and(|total| total != received) {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the download ended early",
            ));
        }
    }
    Ok(())
}

/// `Content-Length`, when the server sent it.
fn content_length(request: &Handle) -> Option<u64> {
    use windows_sys::Win32::Networking::WinHttp::WINHTTP_QUERY_CONTENT_LENGTH;
    // As text: the numeric form is only 32 bits.
    let mut text = [0u16; 32];
    let mut size = (text.len() * 2) as u32;
    // SAFETY: the buffer and its size in bytes match.
    let found = unsafe {
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_CONTENT_LENGTH,
            null(),
            text.as_mut_ptr().cast(),
            &mut size,
            null_mut(),
        )
    } != 0;
    if !found {
        return None;
    }
    String::from_utf16_lossy(&text[..size as usize / 2])
        .trim()
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// Answers one request on 127.0.0.1 with `response`; the URL to ask.
    fn serve_once(response: Vec<u8>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request);
            stream.write_all(&response).unwrap();
        });
        format!("http://127.0.0.1:{port}/feed/latest.json?x=1")
    }

    fn ok(body: &[u8]) -> Vec<u8> {
        let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        response.extend_from_slice(body);
        response
    }

    fn fetch(url: &str, limit: u64) -> io::Result<Vec<u8>> {
        let mut body = Vec::new();
        get(url, limit, |chunk, _| {
            body.extend_from_slice(chunk);
            Ok(())
        })?;
        Ok(body)
    }

    #[test]
    fn reads_a_body() {
        let body: Vec<u8> = (0..200_000u32).map(|i| i as u8).collect();
        let url = serve_once(ok(&body));
        assert_eq!(fetch(&url, 1 << 20).unwrap(), body);
    }

    #[test]
    fn refuses_errors_and_oversized_bodies() {
        let url = serve_once(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_vec());
        assert!(fetch(&url, 1024).is_err());
        let url = serve_once(ok(&[7u8; 2048]));
        assert!(fetch(&url, 1024).is_err());
    }

    #[test]
    fn refuses_plain_http_elsewhere() {
        assert!(crack("http://example.com/latest.json").is_err());
        assert!(crack("ftp://127.0.0.1/x").is_err());
        let target = crack("https://github.com/a/b?c=d").unwrap();
        assert!(target.secure);
        assert_eq!(target.port, 443);
        assert_eq!(String::from_utf16_lossy(&target.object), "/a/b?c=d\0");
    }
}
