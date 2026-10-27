//! OLE virtual files. Descriptor queries are local; only IStream::Read
//! acquires a source lease and requests bounded network data.
#![allow(non_snake_case)]
use super::{ClipboardHandle, register};
use linkcursor_core::{Core, FileManifest, MAX_READ_SIZE, RemoteFileLease};
use std::collections::HashSet;
use std::ffi::c_void;
use std::mem::{ManuallyDrop, size_of};
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Memory::*;
use windows::Win32::UI::Shell::*;
use windows::core::{BOOL, HRESULT, PWSTR, Ref, Result, implement};

struct Files {
    manifest: FileManifest,
    peer: String,
    core: Weak<Core>,
    sharing: ClipboardHandle,
    lease: Mutex<Option<Arc<RemoteFileLease>>>,
    completed: Mutex<HashSet<usize>>,
    streams: AtomicUsize,
    in_operation: AtomicBool,
    failed: AtomicBool,
}
impl Files {
    fn read(
        &self,
        entry: usize,
        offset: u64,
        length: usize,
    ) -> std::result::Result<Vec<u8>, String> {
        if !self.sharing.enabled() || self.failed.load(Ordering::Acquire) {
            return Err("file operation cancelled".into());
        }
        let lease = {
            let mut slot = self.lease.lock().unwrap();
            if slot.is_none() {
                let core = self.core.upgrade().ok_or("core stopped")?;
                *slot = Some(Arc::new(
                    core.open_remote_files(&self.peer, self.manifest.transfer)?,
                ));
            }
            slot.as_ref().unwrap().clone()
        };
        lease.read(entry as u32, offset, length)
    }
    fn finish(&self, reason: &str) {
        if let Some(lease) = self.lease.lock().unwrap().take() {
            lease.finish(reason);
        }
    }
    fn stream(&self) {
        self.streams.fetch_add(1, Ordering::Relaxed);
    }
    fn stream_dropped(&self) {
        if self.streams.fetch_sub(1, Ordering::AcqRel) == 1
            && !self.in_operation.load(Ordering::Acquire)
            && self.completed.lock().unwrap().len()
                == self.manifest.entries.iter().filter(|e| !e.dir).count()
        {
            self.finish("delivered");
        }
    }
}

/// Constructed on the clipboard STA. No file contents are fetched here.
pub(super) fn data_object(
    manifest: FileManifest,
    peer: String,
    core: Weak<Core>,
    sharing: ClipboardHandle,
) -> Result<IDataObject> {
    manifest
        .validate()
        .map_err(|e| windows::core::Error::new(E_INVALIDARG, e))?;
    let descriptors = descriptors(&manifest)?;
    let files = Arc::new(Files {
        manifest,
        peer,
        core,
        sharing,
        lease: Mutex::new(None),
        completed: Mutex::new(HashSet::new()),
        streams: AtomicUsize::new(0),
        in_operation: AtomicBool::new(false),
        failed: AtomicBool::new(false),
    });
    Ok(VirtualFiles {
        files,
        descriptors,
        async_mode: AtomicBool::new(true),
        descriptor: register("FileGroupDescriptorW") as u16,
        contents: register("FileContents") as u16,
        effect: register("Preferred DropEffect") as u16,
        marker: register("LinkCursor Virtual Files") as u16,
    }
    .into())
}

fn descriptors(manifest: &FileManifest) -> Result<Vec<u8>> {
    let mut bytes =
        vec![0; size_of::<u32>() + manifest.entries.len() * size_of::<FILEDESCRIPTORW>()];
    bytes[..4].copy_from_slice(&(manifest.entries.len() as u32).to_le_bytes());
    let mut names = HashSet::new();
    for (i, entry) in manifest.entries.iter().enumerate() {
        // Windows shell descriptors have MAX_PATH storage, including NUL.
        // Reject rather than truncate or silently alias two source entries.
        let path = entry.path.replace('/', "\\");
        let units: Vec<_> = path.encode_utf16().collect();
        if units.len() >= 260 || !names.insert(path.to_lowercase()) || !valid_windows_path(&path) {
            return Err(windows::core::Error::new(
                E_INVALIDARG,
                "File name cannot be represented on Windows",
            ));
        }
        let mut descriptor = FILEDESCRIPTORW {
            dwFlags: (FD_ATTRIBUTES.0 | FD_FILESIZE.0 | FD_PROGRESSUI.0 | FD_UNICODE.0) as u32,
            dwFileAttributes: if entry.dir { 0x10 } else { 0x80 },
            nFileSizeHigh: (entry.size >> 32) as u32,
            nFileSizeLow: entry.size as u32,
            ..Default::default()
        };
        let mut name = [0; 260];
        name[..units.len()].copy_from_slice(&units);
        descriptor.cFileName = name;
        // The count is 4 bytes; FILEDESCRIPTORW can require wider alignment.
        unsafe {
            std::ptr::write_unaligned(
                bytes
                    .as_mut_ptr()
                    .add(4 + i * size_of::<FILEDESCRIPTORW>())
                    .cast(),
                descriptor,
            );
        }
    }
    Ok(bytes)
}

fn valid_windows_path(path: &str) -> bool {
    path.split('\\').all(|part| {
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.ends_with(['.', ' '])
            || part
                .chars()
                .any(|c| c.is_control() || "<>:\"/|?*".contains(c))
        {
            return false;
        }
        let stem = part.split('.').next().unwrap_or("").to_uppercase();
        let numbered_device = (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(
                &stem[3..],
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            );
        !matches!(
            stem.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$" | "CONIN$" | "CONOUT$"
        ) && !numbered_device
    })
}

fn hglobal(bytes: &[u8]) -> Result<STGMEDIUM> {
    unsafe {
        let memory = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1))?;
        let target = GlobalLock(memory);
        if target.is_null() {
            let _ = GlobalFree(Some(memory));
            return Err(E_OUTOFMEMORY.into());
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), target.cast(), bytes.len());
        let _ = GlobalUnlock(memory);
        Ok(STGMEDIUM {
            tymed: TYMED_HGLOBAL.0 as u32,
            u: STGMEDIUM_0 { hGlobal: memory },
            pUnkForRelease: ManuallyDrop::new(None),
        })
    }
}
fn fmt(format: u16, medium: TYMED, index: i32) -> FORMATETC {
    FORMATETC {
        cfFormat: format,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: index,
        tymed: medium.0 as u32,
    }
}

#[implement(IDataObject, IDataObjectAsyncCapability, IAgileObject)]
struct VirtualFiles {
    files: Arc<Files>,
    descriptors: Vec<u8>,
    async_mode: AtomicBool,
    descriptor: u16,
    contents: u16,
    effect: u16,
    marker: u16,
}
impl IAgileObject_Impl for VirtualFiles_Impl {}
impl IDataObject_Impl for VirtualFiles_Impl {
    fn QueryGetData(&self, format: *const FORMATETC) -> HRESULT {
        let Some(format) = (unsafe { format.as_ref() }) else {
            return E_POINTER;
        };
        if format.dwAspect != DVASPECT_CONTENT.0 {
            return DV_E_DVASPECT;
        }
        if format.cfFormat == self.contents {
            if format.lindex < -1
                || (format.lindex >= 0
                    && self
                        .files
                        .manifest
                        .entries
                        .get(format.lindex as usize)
                        .is_none_or(|e| e.dir))
            {
                return DV_E_LINDEX;
            }
            if format.tymed & TYMED_ISTREAM.0 as u32 == 0 {
                return DV_E_TYMED;
            }
        } else if [self.descriptor, self.effect, self.marker].contains(&format.cfFormat) {
            if format.lindex != -1 {
                return DV_E_LINDEX;
            }
            if format.tymed & TYMED_HGLOBAL.0 as u32 == 0 {
                return DV_E_TYMED;
            }
        } else {
            return DV_E_FORMATETC;
        }
        S_OK
    }
    fn GetData(&self, format: *const FORMATETC) -> Result<STGMEDIUM> {
        self.QueryGetData(format).ok()?;
        let format = unsafe { &*format };
        if format.cfFormat == self.descriptor {
            return hglobal(&self.descriptors);
        }
        if format.cfFormat == self.effect {
            return hglobal(&1u32.to_le_bytes());
        }
        if format.cfFormat == self.marker {
            return hglobal(&[1]);
        }
        if format.lindex < 0 {
            return Err(DV_E_LINDEX.into());
        }
        let entry = format.lindex as usize;
        self.files.stream();
        let stream: IStream = RemoteStream {
            files: self.files.clone(),
            entry,
            position: Mutex::new(0),
        }
        .into();
        Ok(STGMEDIUM {
            tymed: TYMED_ISTREAM.0 as u32,
            u: STGMEDIUM_0 {
                pstm: ManuallyDrop::new(Some(stream)),
            },
            pUnkForRelease: ManuallyDrop::new(None),
        })
    }
    fn GetDataHere(&self, _: *const FORMATETC, _: *mut STGMEDIUM) -> Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn GetCanonicalFormatEtc(&self, _: *const FORMATETC, output: *mut FORMATETC) -> HRESULT {
        if output.is_null() {
            return E_POINTER;
        }
        unsafe {
            (*output).ptd = std::ptr::null_mut();
        }
        DATA_S_SAMEFORMATETC
    }
    fn SetData(
        &self,
        format: *const FORMATETC,
        medium: *const STGMEDIUM,
        release: BOOL,
    ) -> Result<()> {
        // Shell reports performed effects; never implement delete-on-paste.
        let Some(format) = (unsafe { format.as_ref() }) else {
            return Err(E_POINTER.into());
        };
        if medium.is_null() {
            return Err(E_POINTER.into());
        }
        if ![
            register("Performed DropEffect") as u16,
            register("Logical Performed DropEffect") as u16,
            register("Paste Succeeded") as u16,
        ]
        .contains(&format.cfFormat)
        {
            return Err(E_NOTIMPL.into());
        }
        if release.as_bool() {
            unsafe {
                windows::Win32::System::Ole::ReleaseStgMedium(medium.cast_mut());
            }
        }
        Ok(())
    }
    fn EnumFormatEtc(&self, direction: u32) -> Result<IEnumFORMATETC> {
        if direction != DATADIR_GET.0 as u32 {
            return Err(E_NOTIMPL.into());
        }
        unsafe {
            SHCreateStdEnumFmtEtc(&[
                fmt(self.descriptor, TYMED_HGLOBAL, -1),
                fmt(self.contents, TYMED_ISTREAM, -1),
                fmt(self.effect, TYMED_HGLOBAL, -1),
                fmt(self.marker, TYMED_HGLOBAL, -1),
            ])
        }
    }
    fn DAdvise(&self, _: *const FORMATETC, _: u32, _: Ref<IAdviseSink>) -> Result<u32> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }
    fn DUnadvise(&self, _: u32) -> Result<()> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }
    fn EnumDAdvise(&self) -> Result<IEnumSTATDATA> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }
}
impl IDataObjectAsyncCapability_Impl for VirtualFiles_Impl {
    fn SetAsyncMode(&self, value: BOOL) -> Result<()> {
        self.async_mode.store(value.as_bool(), Ordering::Release);
        Ok(())
    }
    fn GetAsyncMode(&self) -> Result<BOOL> {
        Ok(self.async_mode.load(Ordering::Acquire).into())
    }
    fn StartOperation(&self, _: Ref<IBindCtx>) -> Result<()> {
        if self.files.in_operation.swap(true, Ordering::AcqRel) {
            return Err(E_UNEXPECTED.into());
        }
        self.files.failed.store(false, Ordering::Release);
        self.files.completed.lock().unwrap().clear();
        Ok(())
    }
    fn InOperation(&self) -> Result<BOOL> {
        Ok(self.files.in_operation.load(Ordering::Acquire).into())
    }
    fn EndOperation(&self, result: HRESULT, _: Ref<IBindCtx>, _: u32) -> Result<()> {
        self.files.in_operation.store(false, Ordering::Release);
        self.files.failed.store(result.is_err(), Ordering::Release);
        self.files
            .finish(if result.is_ok() { "done" } else { "cancelled" });
        Ok(())
    }
}

#[implement(IStream, IAgileObject)]
struct RemoteStream {
    files: Arc<Files>,
    entry: usize,
    position: Mutex<u64>,
}
impl Drop for RemoteStream {
    fn drop(&mut self) {
        self.files.stream_dropped();
    }
}
impl IAgileObject_Impl for RemoteStream_Impl {}
impl ISequentialStream_Impl for RemoteStream_Impl {
    fn Read(&self, target: *mut c_void, count: u32, read: *mut u32) -> HRESULT {
        if !read.is_null() {
            unsafe {
                *read = 0;
            }
        }
        if count > 0 && target.is_null() {
            return E_POINTER;
        }
        let mut position = self.position.lock().unwrap();
        let size = self.files.manifest.entries[self.entry].size;
        let wanted = u64::from(count).min(size.saturating_sub(*position)) as usize;
        let mut total = 0;
        while total < wanted {
            match self
                .files
                .read(self.entry, *position, (wanted - total).min(MAX_READ_SIZE))
            {
                Ok(bytes) if !bytes.is_empty() && bytes.len() <= wanted - total => {
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            bytes.as_ptr(),
                            target.cast::<u8>().add(total),
                            bytes.len(),
                        );
                    }
                    total += bytes.len();
                    *position += bytes.len() as u64;
                    if !read.is_null() {
                        unsafe {
                            *read = total as u32;
                        }
                    }
                }
                outcome => {
                    let reason = outcome
                        .err()
                        .unwrap_or_else(|| "incomplete remote file read".into());
                    log!("virtual file read failed: {reason}");
                    self.files.failed.store(true, Ordering::Release);
                    self.files.finish("read failed");
                    return STG_E_READFAULT;
                }
            }
        }
        if *position == size {
            self.files.completed.lock().unwrap().insert(self.entry);
        }
        if total == count as usize {
            S_OK
        } else {
            S_FALSE
        }
    }
    fn Write(&self, _: *const c_void, _: u32, written: *mut u32) -> HRESULT {
        if !written.is_null() {
            unsafe {
                *written = 0;
            }
        }
        STG_E_ACCESSDENIED
    }
}
impl IStream_Impl for RemoteStream_Impl {
    fn Seek(&self, delta: i64, origin: STREAM_SEEK, output: *mut u64) -> Result<()> {
        let mut position = self.position.lock().unwrap();
        let base = match origin {
            STREAM_SEEK_SET => 0,
            STREAM_SEEK_CUR => *position,
            STREAM_SEEK_END => self.files.manifest.entries[self.entry].size,
            _ => return Err(STG_E_INVALIDFUNCTION.into()),
        };
        let next = i128::from(base) + i128::from(delta);
        if !(0..=i128::from(u64::MAX)).contains(&next) {
            return Err(STG_E_INVALIDFUNCTION.into());
        }
        *position = next as u64;
        if !output.is_null() {
            unsafe {
                *output = *position;
            }
        }
        Ok(())
    }
    fn SetSize(&self, _: u64) -> Result<()> {
        Err(STG_E_ACCESSDENIED.into())
    }
    fn CopyTo(
        &self,
        target: Ref<IStream>,
        count: u64,
        read: *mut u64,
        written: *mut u64,
    ) -> Result<()> {
        if !read.is_null() {
            unsafe {
                *read = 0;
            }
        }
        if !written.is_null() {
            unsafe {
                *written = 0;
            }
        }
        let target = target.as_ref().ok_or(E_POINTER)?;
        let mut buffer = vec![0u8; MAX_READ_SIZE];
        let mut total_read = 0;
        let mut total_written = 0;
        while total_read < count {
            let mut received = 0;
            let result = self.Read(
                buffer.as_mut_ptr().cast(),
                (count - total_read).min(MAX_READ_SIZE as u64) as u32,
                &mut received,
            );
            total_read += u64::from(received);
            if !read.is_null() {
                unsafe {
                    *read = total_read;
                }
            }
            if received > 0 {
                let mut sent = 0;
                let output =
                    unsafe { target.Write(buffer.as_ptr().cast(), received, Some(&mut sent)) };
                total_written += u64::from(sent);
                if !written.is_null() {
                    unsafe {
                        *written = total_written;
                    }
                }
                output.ok()?;
                if sent != received {
                    return Err(STG_E_MEDIUMFULL.into());
                }
            }
            result.ok()?;
            if result == S_FALSE || received == 0 {
                break;
            }
        }
        Ok(())
    }
    fn Commit(&self, _: &STGC) -> Result<()> {
        Ok(())
    }
    fn Revert(&self) -> Result<()> {
        Err(STG_E_INVALIDFUNCTION.into())
    }
    fn LockRegion(&self, _: u64, _: u64, _: &LOCKTYPE) -> Result<()> {
        Err(STG_E_INVALIDFUNCTION.into())
    }
    fn UnlockRegion(&self, _: u64, _: u64, _: u32) -> Result<()> {
        Err(STG_E_INVALIDFUNCTION.into())
    }
    fn Stat(&self, output: *mut STATSTG, flags: &STATFLAG) -> Result<()> {
        if output.is_null() {
            return Err(E_POINTER.into());
        }
        let entry = &self.files.manifest.entries[self.entry];
        let mut stat = STATSTG {
            r#type: STGTY_STREAM.0 as u32,
            cbSize: entry.size,
            grfMode: STGM_READ,
            ..Default::default()
        };
        if *flags != STATFLAG_NONAME {
            let units: Vec<u16> = entry.path.encode_utf16().chain(Some(0)).collect();
            let name = unsafe { CoTaskMemAlloc(units.len() * 2).cast::<u16>() };
            if name.is_null() {
                return Err(E_OUTOFMEMORY.into());
            }
            unsafe {
                std::ptr::copy_nonoverlapping(units.as_ptr(), name, units.len());
            }
            stat.pwcsName = PWSTR(name);
        }
        unsafe {
            *output = stat;
        }
        Ok(())
    }
    fn Clone(&self) -> Result<IStream> {
        self.files.stream();
        Ok(RemoteStream {
            files: self.files.clone(),
            entry: self.entry,
            position: Mutex::new(*self.position.lock().unwrap()),
        }
        .into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use linkcursor_core::{
        Clipboard, ClipboardFormat, CoreConfig, FileEntry, LC_INPUT_MOUSE_MOVE, LcInputEvent,
        MIME_TEXT, MIME_URI_LIST,
    };
    use std::time::{Duration, Instant};
    use windows::core::{Interface, PCWSTR};

    #[test]
    fn descriptors_preserve_large_sizes_and_reject_ambiguous_names() {
        let mut manifest = FileManifest {
            transfer: 1,
            total_size: (1 << 33) + 19,
            entries: vec![FileEntry {
                path: "报告.bin".into(),
                size: (1 << 33) + 19,
                dir: false,
            }],
        };
        let bytes = descriptors(&manifest).unwrap();
        let descriptor =
            unsafe { std::ptr::read_unaligned(bytes.as_ptr().add(4).cast::<FILEDESCRIPTORW>()) };
        let high = descriptor.nFileSizeHigh;
        let low = descriptor.nFileSizeLow;
        assert_eq!(
            (u64::from(high) << 32) | u64::from(low),
            manifest.total_size
        );
        for path in ["CON.txt", "a:stream", "a\\..\\b", "COM¹.txt", "x. "] {
            manifest.entries[0].path = path.into();
            assert!(descriptors(&manifest).is_err(), "{path}");
        }
        manifest.entries[0].path = "ok.txt".into();
        manifest.entries.push(FileEntry {
            path: "OK.txt".into(),
            size: 1,
            dir: false,
        });
        assert!(descriptors(&manifest).is_err());
    }

    fn port() -> u16 {
        std::net::UdpSocket::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }
    fn event(receiver: &std::sync::mpsc::Receiver<String>, kind: &str) -> serde_json::Value {
        let end = Instant::now() + Duration::from_secs(10);
        let mut seen = vec![];
        loop {
            let raw = receiver
                .recv_timeout(end.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|_| panic!("missing {kind}: {seen:?}"));
            let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
            if value["event"] == kind {
                return value;
            }
            seen.push(value);
        }
    }

    /// Exercises actual COM streams and Windows file storage with two encrypted
    /// cores. The normal run does not touch the system clipboard; the explicit
    /// LINKCURSOR_TEST_CLIPBOARD opt-in supports a manual Explorer smoke test.
    #[test]
    fn virtual_file_streams_copy_and_survive_new_copies() {
        unsafe {
            windows::Win32::System::Ole::OleInitialize(None).unwrap();
        }
        struct Ole;
        impl Drop for Ole {
            fn drop(&mut self) {
                unsafe {
                    windows::Win32::System::Ole::OleUninitialize();
                }
            }
        }
        let _ole = Ole;
        let root = std::env::temp_dir().join(format!(
            "linkcursor-virtual-test-{}-{}",
            std::process::id(),
            port()
        ));
        let report = root.join("报告");
        std::fs::create_dir_all(report.join("empty-folder")).unwrap();
        let bytes: Vec<u8> = (0..900_003).map(|i| (i * 31) as u8).collect();
        std::fs::write(report.join("video.bin"), &bytes).unwrap();
        std::fs::write(report.join("empty.txt"), []).unwrap();
        std::fs::write(root.join("note.txt"), b"note").unwrap();
        let cache = root.join("unused-cache");
        let destination = std::env::var_os("LINKCURSOR_TEST_DESTINATION")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| root.join("destination"));
        std::fs::create_dir_all(&destination).unwrap();
        let discovery_port = port();
        let config = |id: &str| CoreConfig {
            device_id: id.into(),
            device_name: id.into(),
            platform: "windows".into(),
            discovery_port,
            control_port: port(),
            clipboard: true,
            files: true,
            on_demand_files: true,
            download_directory: Some(cache.to_string_lossy().into()),
            auto_connect: false,
            ..Default::default()
        };
        let (source_events, source_rx) = std::sync::mpsc::channel();
        let source = Arc::new(
            Core::start(
                config("ole-source"),
                move |s| {
                    let _ = source_events.send(s);
                },
                |_| {},
            )
            .unwrap(),
        );
        let (target_events, target_rx) = std::sync::mpsc::channel();
        let target = Arc::new(
            Core::start(
                config("ole-target"),
                move |s| {
                    let _ = target_events.send(s);
                },
                |_| {},
            )
            .unwrap(),
        );
        event(&source_rx, "peer_up");
        source.connect("ole-target").unwrap();
        event(&source_rx, "connected");
        event(&target_rx, "connected");
        let urls = [report.clone(), root.join("note.txt")]
            .iter()
            .map(|path| super::super::file_url(&path.to_string_lossy()))
            .collect::<Vec<_>>()
            .join("\r\n");
        source
            .set_clipboard(Clipboard {
                formats: vec![ClipboardFormat {
                    mime: MIME_URI_LIST.into(),
                    data: urls.into_bytes(),
                }],
            })
            .unwrap();
        source
            .send_input(LcInputEvent {
                kind: LC_INPUT_MOUSE_MOVE,
                delta_x: 1.0,
                ..Default::default()
            })
            .unwrap();
        let offered = event(&target_rx, "files_available");
        let manifest: FileManifest = serde_json::from_value(offered["manifest"].clone()).unwrap();
        let video = manifest
            .entries
            .iter()
            .position(|e| e.path.ends_with("video.bin"))
            .unwrap();
        let object = data_object(
            manifest.clone(),
            "ole-source".into(),
            Arc::downgrade(&target),
            ClipboardHandle::new(true),
        )
        .unwrap();
        if std::env::var_os("LINKCURSOR_TEST_CLIPBOARD").is_some() {
            use windows::Win32::System::Ole::{
                OleGetClipboard, OleIsCurrentClipboard, OleSetClipboard,
            };
            use windows::Win32::UI::WindowsAndMessaging::{
                DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
            };
            let previous = unsafe { OleGetClipboard() }.ok();
            unsafe {
                OleSetClipboard(&object).unwrap();
            }
            std::fs::write(
                std::env::var("LINKCURSOR_TEST_DESTINATION_FILE").unwrap(),
                destination.to_string_lossy().as_bytes(),
            )
            .unwrap();
            let deadline = Instant::now() + Duration::from_secs(120);
            while Instant::now() < deadline {
                unsafe {
                    let mut message = MSG::default();
                    while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                        let _ = TranslateMessage(&message);
                        DispatchMessageW(&message);
                    }
                }
                if destination.join("note.txt").exists()
                    && destination
                        .join("报告/video.bin")
                        .metadata()
                        .is_ok_and(|m| m.len() == bytes.len() as u64)
                    && !unsafe {
                        object
                            .cast::<IDataObjectAsyncCapability>()
                            .unwrap()
                            .InOperation()
                            .unwrap()
                    }
                    .as_bool()
                {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            if unsafe { OleIsCurrentClipboard(&object) }.is_ok() {
                let _ = unsafe { OleSetClipboard(previous.as_ref()) };
            }
        } else {
            unsafe {
                object
                    .QueryGetData(&fmt(register("FileContents") as u16, TYMED_ISTREAM, -1))
                    .ok()
                    .unwrap();
                let mut medium = object
                    .GetData(&fmt(
                        register("FileGroupDescriptorW") as u16,
                        TYMED_HGLOBAL,
                        -1,
                    ))
                    .unwrap();
                windows::Win32::System::Ole::ReleaseStgMedium(&mut medium);
                let mut medium = object
                    .GetData(&fmt(
                        register("FileContents") as u16,
                        TYMED_ISTREAM,
                        video as i32,
                    ))
                    .unwrap();
                let stream = medium.u.pstm.as_ref().unwrap().clone();
                let mut stat = STATSTG::default();
                stream.Stat(&mut stat, STATFLAG_NONAME).unwrap();
                assert_eq!(stat.cbSize, bytes.len() as u64);
                stream.Seek(400_000, STREAM_SEEK_SET, None).unwrap();
                windows::Win32::System::Ole::ReleaseStgMedium(&mut medium);
                drop(stream);
            }
            // Merely requesting a descriptor/stream/Stat/Seek did not fetch data.
            assert!(!cache.exists());
            assert!(
                !source_rx
                    .try_iter()
                    .any(|s| s.contains("\"files_sending\""))
            );
            unsafe {
                let operation: IDataObjectAsyncCapability = object.cast().unwrap();
                operation.StartOperation(None).unwrap();
                for (index, entry) in manifest.entries.iter().enumerate() {
                    let path = destination.join(&entry.path);
                    if entry.dir {
                        std::fs::create_dir_all(&path).unwrap();
                        continue;
                    }
                    let mut medium = object
                        .GetData(&fmt(
                            register("FileContents") as u16,
                            TYMED_ISTREAM,
                            index as i32,
                        ))
                        .unwrap();
                    let source_stream = medium.u.pstm.as_ref().unwrap().clone();
                    let name: Vec<u16> = path
                        .to_string_lossy()
                        .encode_utf16()
                        .chain(Some(0))
                        .collect();
                    let output = SHCreateStreamOnFileEx(
                        PCWSTR(name.as_ptr()),
                        (STGM_WRITE | STGM_CREATE | STGM_SHARE_EXCLUSIVE).0,
                        0x80,
                        true,
                        None,
                    )
                    .unwrap();
                    let mut read = 0;
                    let mut written = 0;
                    source_stream
                        .CopyTo(&output, entry.size + 1, Some(&mut read), Some(&mut written))
                        .unwrap();
                    assert_eq!(read, entry.size);
                    assert_eq!(written, entry.size);
                    output.Commit(STGC_DEFAULT).unwrap();
                    windows::Win32::System::Ole::ReleaseStgMedium(&mut medium);
                }
                operation.EndOperation(S_OK, None, 1).unwrap();
            }
        }
        assert_eq!(
            std::fs::read(destination.join("报告/video.bin")).unwrap(),
            bytes
        );
        assert!(destination.join("报告/empty-folder").is_dir());
        assert_eq!(
            std::fs::metadata(destination.join("报告/empty.txt"))
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            std::fs::read(destination.join("note.txt")).unwrap(),
            b"note"
        );
        assert!(!cache.exists());
        // Pin another operation before replacing BOTH clipboards.
        unsafe {
            let operation: IDataObjectAsyncCapability = object.cast().unwrap();
            operation.StartOperation(None).unwrap();
            let mut medium = object
                .GetData(&fmt(
                    register("FileContents") as u16,
                    TYMED_ISTREAM,
                    video as i32,
                ))
                .unwrap();
            let stream = medium.u.pstm.as_ref().unwrap().clone();
            let mut buffer = [0; 17];
            let mut received = 0;
            stream
                .Read(buffer.as_mut_ptr().cast(), 17, Some(&mut received))
                .ok()
                .unwrap();
            assert_eq!(&buffer, &bytes[..17]);
            let text = || Clipboard {
                formats: vec![ClipboardFormat {
                    mime: MIME_TEXT.into(),
                    data: b"new copy".to_vec(),
                }],
            };
            source.set_clipboard(text()).unwrap();
            target.set_clipboard(text()).unwrap();
            stream.Seek(700_000, STREAM_SEEK_SET, None).unwrap();
            let clone = stream.Clone().unwrap();
            clone
                .Read(buffer.as_mut_ptr().cast(), 17, Some(&mut received))
                .ok()
                .unwrap();
            assert_eq!(&buffer, &bytes[700_000..700_017]);
            operation.EndOperation(S_OK, None, 1).unwrap();
            windows::Win32::System::Ole::ReleaseStgMedium(&mut medium);
        }
        drop(object);
        drop(source);
        drop(target);
        std::fs::remove_dir_all(root).unwrap();
    }
}
