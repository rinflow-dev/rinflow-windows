// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Updates: finds a newer version, downloads its setup program in the
//! background, and runs it when the user says so.
//!
//! - **Where.** The release workflow publishes `latest.json` with every
//!   release (`tools/update-signing manifest`); GitHub serves the newest one
//!   at [`FEED_URL`]. Pre-releases are never offered.
//! - **When.** A minute after start-up, then once a day while
//!   **自动检查更新** (`auto_update`) is on; and whenever the user selects
//!   **检查更新**.
//! - **Trust.** Each setup program carries a minisign signature made with the
//!   project's key; [`PUBLIC_KEY`] is built into the agent. The signed
//!   comment names the file and its version, so neither a changed feed nor a
//!   changed download can install anything the key did not sign, nor an
//!   older version offered as newer. Downloads go to
//!   `%LOCALAPPDATA%\Rinflow\updates`, are checked while they arrive, and
//!   are deleted when the signature does not match.
//! - **Installing** runs the setup program quietly (`/VERYSILENT`); Windows
//!   asks for an administrator once (UAC), as for any install into Program
//!   Files. Setup asks this agent to quit (`rinflow-agent --quit`), so peers
//!   see a clean disconnect, replaces the files and starts the agent again
//!   for the user. If the user declines, Rinflow keeps running and the update
//!   stays ready.

mod http;

use crate::i18n::{s, tr, trf};
use crate::settings::SettingsStore;
use crate::state::SharedState;
use crate::{config, identity, tray};
use minisign_verify::{PublicKey, Signature};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The newest release's `latest.json`. `RINFLOW_UPDATE_FEED` replaces it,
/// for trying updates against a test feed: what is installed must still be
/// signed with [`PUBLIC_KEY`].
pub const FEED_URL: &str =
    "https://github.com/rinflow-dev/rinflow-windows/releases/latest/download/latest.json";
/// The key that signs setup programs (minisign; its secret half is the
/// `RINFLOW_UPDATE_KEY` secret of the release workflow).
const PUBLIC_KEY: &str = "RWS1o1LUSmix3TrVdrgLm/oAZ7+7XesvhBmacREHwDYzD/Q1nigJpEqB";
/// This build's setup programs: x64, or ARM64.
const ARCH: &str = if cfg!(target_arch = "aarch64") {
    "arm64"
} else {
    "x64"
};
const CURRENT: &str = env!("CARGO_PKG_VERSION");

const FIRST_CHECK_AFTER: Duration = Duration::from_secs(60);
const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);
/// `latest.json` is a few KB.
const MAX_FEED: u64 = 256 * 1024;
/// A setup program is about 20 MB; anything near this is not one.
const MAX_PACKAGE: u64 = 512 * 1024 * 1024;

/// What the settings page shows about updates.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct UpdateView {
    pub status: UpdateStatus,
    /// The newer version, once one was found.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Its release notes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes_url: Option<String>,
    /// While downloading: 0 to 1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<f64>,
    /// Why the last check, download or install failed, for the user.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateStatus {
    /// Not checked yet.
    #[default]
    Idle,
    Checking,
    UpToDate,
    Downloading,
    /// Downloaded and verified: can be installed.
    Ready,
    /// The setup program runs.
    Installing,
    /// The check or the download failed (`error` says why).
    Failed,
}

/// Requests to the update thread.
enum Request {
    Check,
    Install,
}

/// Sends requests to the update thread. Any thread.
#[derive(Clone)]
pub struct UpdateHandle(Sender<Request>);

impl UpdateHandle {
    pub fn check(&self) {
        let _ = self.0.send(Request::Check);
    }

    pub fn install(&self) {
        let _ = self.0.send(Request::Install);
    }
}

/// The running updater, for the tray menu.
static HANDLE: OnceLock<UpdateHandle> = OnceLock::new();
/// The version ready to install, for the tray menu.
static READY: Mutex<Option<String>> = Mutex::new(None);

/// The version that can be installed now, if any.
pub fn ready_version() -> Option<String> {
    READY.lock().ok()?.clone()
}

/// Installs the update that is ready (the tray menu's 安装更新).
pub fn install() {
    if let Some(handle) = HANDLE.get() {
        handle.install();
    }
}

/// Starts the update thread. Without it Rinflow works on; it only does not
/// update itself.
pub fn start(state: SharedState, settings: Arc<SettingsStore>) -> UpdateHandle {
    let (sender, receiver) = mpsc::channel();
    let handle = UpdateHandle(sender);
    let _ = HANDLE.set(handle.clone());
    let started = std::thread::Builder::new()
        .name("update".to_owned())
        .spawn(move || {
            let mut updater = Updater {
                state,
                view: UpdateView::default(),
                ready: None,
                notified: None,
            };
            updater.announce_installed();
            let mut next = Instant::now() + FIRST_CHECK_AFTER;
            loop {
                let wait = next.saturating_duration_since(Instant::now());
                match receiver.recv_timeout(wait) {
                    Ok(Request::Check) => updater.check(true),
                    Ok(Request::Install) => updater.install(),
                    Err(RecvTimeoutError::Timeout) => {
                        if settings.get().auto_update {
                            updater.check(false);
                        }
                        next = Instant::now() + CHECK_EVERY;
                    }
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        });
    if let Err(error) = started {
        log!("cannot start the updater: {error}");
    }
    handle
}

/// A newer version, as the feed offers it for this PC.
#[derive(Debug)]
struct Offer {
    version: String,
    notes_url: Option<String>,
    url: String,
    size: u64,
    signature: String,
    /// `Rinflow-Setup-<version>-<arch>.exe`.
    file_name: String,
}

#[derive(Deserialize)]
struct Feed {
    version: String,
    #[serde(default)]
    notes_url: Option<String>,
    #[serde(default)]
    platforms: BTreeMap<String, Package>,
}

#[derive(Deserialize)]
struct Package {
    url: String,
    size: u64,
    signature: String,
}

struct Updater {
    state: SharedState,
    view: UpdateView,
    /// The verified setup program.
    ready: Option<Ready>,
    /// The version the user was told about.
    notified: Option<String>,
}

#[derive(Clone)]
struct Ready {
    version: String,
    path: PathBuf,
    /// Checked again just before it runs: the file sat in the user's
    /// folder meanwhile.
    signature: String,
}

impl Updater {
    fn show(&mut self, view: UpdateView) {
        self.view = view.clone();
        self.state.set_update(view);
    }

    fn fail(&mut self, error: String) {
        log!("update: {error}");
        let view = UpdateView {
            status: UpdateStatus::Failed,
            error: Some(error),
            ..self.view.clone()
        };
        self.show(view);
    }

    /// Looks for a newer version and downloads it. A check the user did
    /// not ask for fails quietly (no network, say): only the log says so.
    fn check(&mut self, asked: bool) {
        if matches!(
            self.view.status,
            UpdateStatus::Downloading | UpdateStatus::Installing
        ) {
            return;
        }
        if asked {
            let view = UpdateView {
                status: UpdateStatus::Checking,
                ..UpdateView::default()
            };
            self.show(view);
        }
        let offer = match fetch_offer() {
            Ok(offer) => offer,
            Err(error) => {
                if asked {
                    self.fail(trf(s::UpdateCheckFailed, &[&error]));
                } else {
                    log!("update check failed: {error}");
                }
                return;
            }
        };
        let Some(offer) = offer else {
            self.set_ready(None);
            remove_downloads();
            self.show(UpdateView {
                status: UpdateStatus::UpToDate,
                ..UpdateView::default()
            });
            return;
        };
        if self
            .ready
            .as_ref()
            .is_some_and(|ready| ready.version == offer.version && ready.path.exists())
        {
            self.show_ready(&offer);
            return;
        }
        let directory = updates_directory();
        let path = directory.join(&offer.file_name);
        // A copy downloaded before a restart is used if it still checks out.
        if verify_file(&path, &offer.signature, PUBLIC_KEY).is_ok() {
            self.downloaded(&offer, path);
            return;
        }
        remove_downloads();
        log!("update: downloading {}", offer.version);
        let base = UpdateView {
            status: UpdateStatus::Downloading,
            version: Some(offer.version.clone()),
            notes_url: offer.notes_url.clone(),
            progress: Some(0.0),
            error: None,
        };
        self.show(base.clone());
        let mut shown = 0.0;
        let result = download(&offer, &directory, PUBLIC_KEY, |progress| {
            // Whole percents only: every snapshot goes to the app.
            if progress - shown >= 0.01 {
                shown = progress;
                self.show(UpdateView {
                    progress: Some(progress),
                    ..base.clone()
                });
            }
        });
        match result {
            Ok(path) => self.downloaded(&offer, path),
            Err(error) => {
                let message = match error {
                    DownloadError::Signature => tr(s::UpdateBadSignature).to_owned(),
                    DownloadError::Io(error) => trf(s::UpdateDownloadFailed, &[&error]),
                };
                if asked {
                    self.fail(message);
                } else {
                    log!("update: {message}");
                    self.show(UpdateView::default());
                }
            }
        }
    }

    fn downloaded(&mut self, offer: &Offer, path: PathBuf) {
        log!("update: {} is ready", offer.version);
        self.ready = Some(Ready {
            version: offer.version.clone(),
            path,
            signature: offer.signature.clone(),
        });
        self.set_ready(Some(offer.version.clone()));
        self.show_ready(offer);
        if self.notified.as_deref() != Some(&offer.version) {
            self.notified = Some(offer.version.clone());
            tray::notify(
                &trf(s::UpdateReadyTitle, &[&offer.version]),
                tr(s::UpdateReadyText),
            );
        }
    }

    fn show_ready(&mut self, offer: &Offer) {
        self.show(UpdateView {
            status: UpdateStatus::Ready,
            version: Some(offer.version.clone()),
            notes_url: offer.notes_url.clone(),
            progress: None,
            error: None,
        });
    }

    fn set_ready(&self, version: Option<String>) {
        if let Ok(mut ready) = READY.lock() {
            *ready = version;
        }
    }

    /// Runs the setup program. It asks for an administrator, asks this
    /// agent to quit, and starts the new one: this thread then ends with
    /// the process. If setup ends while the agent still runs, the user
    /// declined or it failed: the update stays ready.
    fn install(&mut self) {
        if self.view.status == UpdateStatus::Installing {
            return;
        }
        let Some(Ready {
            version,
            path,
            signature,
        }) = self.ready.clone()
        else {
            return;
        };
        if verify_file(&path, &signature, PUBLIC_KEY).is_err() {
            let _ = std::fs::remove_file(&path);
            self.ready = None;
            self.set_ready(None);
            self.fail(tr(s::UpdateBadSignature).to_owned());
            return;
        }
        let log = updates_directory().join("setup.log");
        log!("update: installing {version}");
        let ready_view = self.view.clone();
        self.show(UpdateView {
            status: UpdateStatus::Installing,
            ..ready_view.clone()
        });
        // The click that asked for this gave the settings app (or the tray
        // menu) the foreground; passed on to setup, Windows shows its
        // administrator prompt in front instead of flashing in the taskbar.
        // SAFETY: plain Win32 call.
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(
                windows_sys::Win32::UI::WindowsAndMessaging::ASFW_ANY,
            );
        }
        let started = std::process::Command::new(&path)
            .args([
                "/VERYSILENT",
                "/SUPPRESSMSGBOXES",
                "/NORESTART",
                "/SP-",
                // Setup starts the agent again afterwards, and leaves
                // starting at sign-in as the user set it.
                "/RinflowUpdate=1",
            ])
            .arg(format!("/LOG={}", log.display()))
            .spawn();
        let mut child = match started {
            Ok(child) => child,
            Err(error) => {
                self.show(UpdateView {
                    error: Some(trf(s::UpdateStartFailed, &[&error])),
                    ..ready_view
                });
                return;
            }
        };
        let code = child.wait().ok().and_then(|status| status.code());
        log!("update: setup ended ({code:?}) and this agent still runs");
        self.show(UpdateView {
            error: Some(trf(s::UpdateNotInstalled, &[&log.display()])),
            ..ready_view
        });
    }

    /// Says once that Rinflow was updated, on the first start of a new
    /// version, and remembers the version.
    fn announce_installed(&self) {
        let previous = config::section(config::UPDATE)
            .and_then(|section| section["last_version"].as_str().map(str::to_owned));
        if previous.as_deref() == Some(CURRENT) {
            return;
        }
        config::stage(
            config::UPDATE,
            &serde_json::json!({ "last_version": CURRENT }),
        )
        .write();
        remove_downloads();
        // The very first start is not an update.
        if previous.is_some() {
            // The tray icon is added after the agent's other parts start.
            std::thread::sleep(Duration::from_secs(5));
            tray::notify(tr(s::UpdatedTitle), &trf(s::UpdatedText, &[&CURRENT]));
        }
    }
}

fn feed_url() -> String {
    std::env::var("RINFLOW_UPDATE_FEED").unwrap_or_else(|_| FEED_URL.to_owned())
}

fn updates_directory() -> PathBuf {
    identity::data_directory().join("updates")
}

/// The newer version the feed offers this PC, if any.
fn fetch_offer() -> Result<Option<Offer>, String> {
    let mut feed = Vec::new();
    http::get(&feed_url(), MAX_FEED, |chunk, _| {
        feed.extend_from_slice(chunk);
        Ok(())
    })
    .map_err(|error| error.to_string())?;
    offer_from(&feed, CURRENT, ARCH)
}

/// Reads the feed: `None` when it offers nothing newer than `current`.
fn offer_from(feed: &[u8], current: &str, arch: &str) -> Result<Option<Offer>, String> {
    let feed: Feed =
        serde_json::from_slice(feed).map_err(|_| tr(s::UpdateFeedInvalid).to_owned())?;
    let (Some(offered), Some(running)) = (parse_version(&feed.version), parse_version(current))
    else {
        return Err(tr(s::UpdateFeedInvalid).to_owned());
    };
    if offered <= running {
        return Ok(None);
    }
    let package = feed
        .platforms
        .get(arch)
        .ok_or_else(|| tr(s::UpdateNoPackage).to_owned())?;
    let file_name = format!("Rinflow-Setup-{}-{arch}.exe", feed.version);
    // The name the signature vouches for; the URL must lead to it.
    if !package.url.ends_with(&format!("/{file_name}")) || package.size > MAX_PACKAGE {
        return Err(tr(s::UpdateFeedInvalid).to_owned());
    }
    Ok(Some(Offer {
        version: feed.version,
        notes_url: feed.notes_url.filter(|url| url.starts_with("https://")),
        url: package.url.clone(),
        size: package.size,
        signature: package.signature.clone(),
        file_name,
    }))
}

/// `major.minor.patch`; a pre-release suffix (`-beta.1`) is ignored.
fn parse_version(version: &str) -> Option<(u64, u64, u64)> {
    let core = version.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|part| part.parse::<u64>().ok());
    let version = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(version)
}

#[derive(Debug)]
enum DownloadError {
    /// The file is not what the key signed (it is deleted).
    Signature,
    Io(io::Error),
}

/// The signature, checked to vouch for this very file and version.
fn signature_for(
    signature: &str,
    public_key: &str,
    file_name: &str,
    version: &str,
) -> Option<(PublicKey, Signature)> {
    let key = PublicKey::from_base64(public_key).ok()?;
    let signature = Signature::decode(signature).ok()?;
    let mut file = false;
    let mut signed_version = false;
    for field in signature.trusted_comment().split('\t') {
        file |= field == format!("file:{file_name}");
        signed_version |= field == format!("version:{version}");
    }
    (file && signed_version).then_some((key, signature))
}

/// Downloads the offer into `directory`, checking its signature on the way;
/// `progress` gets 0 to 1. The file only takes its name once it checks out.
fn download(
    offer: &Offer,
    directory: &Path,
    public_key: &str,
    mut progress: impl FnMut(f64),
) -> Result<PathBuf, DownloadError> {
    let (key, signature) = signature_for(
        &offer.signature,
        public_key,
        &offer.file_name,
        &offer.version,
    )
    .ok_or(DownloadError::Signature)?;
    let mut verifier = key
        .verify_stream(&signature)
        .map_err(|_| DownloadError::Signature)?;
    std::fs::create_dir_all(directory).map_err(DownloadError::Io)?;
    let path = directory.join(&offer.file_name);
    let partial = directory.join(format!("{}.partial", offer.file_name));
    let result = (|| {
        let mut file = std::fs::File::create(&partial)?;
        let mut received = 0u64;
        http::get(&offer.url, offer.size, |chunk, _| {
            verifier.update(chunk);
            file.write_all(chunk)?;
            received += chunk.len() as u64;
            progress(received as f64 / offer.size.max(1) as f64);
            Ok(())
        })?;
        if received != offer.size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the download has another size than the release lists",
            ));
        }
        file.sync_all()
    })();
    if let Err(error) = result {
        let _ = std::fs::remove_file(&partial);
        return Err(DownloadError::Io(error));
    }
    if verifier.finalize().is_err() {
        let _ = std::fs::remove_file(&partial);
        return Err(DownloadError::Signature);
    }
    std::fs::rename(&partial, &path).map_err(DownloadError::Io)?;
    Ok(path)
}

/// Checks a downloaded setup program against its signature.
fn verify_file(path: &Path, signature: &str, public_key: &str) -> Result<(), DownloadError> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(DownloadError::Signature)?;
    let version = file_name
        .strip_prefix("Rinflow-Setup-")
        .and_then(|rest| rest.strip_suffix(&format!("-{ARCH}.exe")))
        .ok_or(DownloadError::Signature)?;
    let (key, signature) =
        signature_for(signature, public_key, file_name, version).ok_or(DownloadError::Signature)?;
    let mut verifier = key
        .verify_stream(&signature)
        .map_err(|_| DownloadError::Signature)?;
    let mut file = std::fs::File::open(path).map_err(DownloadError::Io)?;
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(DownloadError::Io)?;
        if read == 0 {
            break;
        }
        verifier.update(&buffer[..read]);
    }
    verifier.finalize().map_err(|_| DownloadError::Signature)
}

/// Deletes downloaded setup programs and the setup log.
fn remove_downloads() {
    let Ok(entries) = std::fs::read_dir(updates_directory()) else {
        return;
    };
    for entry in entries.flatten() {
        let _ = std::fs::remove_file(entry.path());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minisign::KeyPair;

    struct Keys {
        public: String,
        pair: KeyPair,
    }

    fn keys() -> Keys {
        let pair = KeyPair::generate_unencrypted_keypair().unwrap();
        Keys {
            public: pair.pk.to_base64(),
            pair,
        }
    }

    fn sign(keys: &Keys, data: &[u8], comment: &str) -> String {
        minisign::sign(None, &keys.pair.sk, data, Some(comment), None)
            .unwrap()
            .into_string()
    }

    fn feed(version: &str, signature: &str, size: usize) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "version": version,
            "notes_url": "https://github.com/rinflow-dev/rinflow-windows/releases/tag/v0.0.2",
            "platforms": {
                "x64": {
                    "url": format!("https://example.com/download/Rinflow-Setup-{version}-x64.exe"),
                    "size": size,
                    "signature": signature,
                }
            }
        }))
        .unwrap()
    }

    #[test]
    fn versions_compare_by_number() {
        assert!(parse_version("0.0.10") > parse_version("0.0.9"));
        assert!(parse_version("1.0.0") > parse_version("0.9.99"));
        assert_eq!(parse_version("0.1.0-beta.2"), Some((0, 1, 0)));
        assert_eq!(parse_version("0.1"), None);
        assert_eq!(parse_version("0.1.0.0"), None);
        assert_eq!(parse_version("v0.1.0"), None);
    }

    #[test]
    fn only_a_newer_version_for_this_pc_is_offered() {
        let feed = feed("0.0.2", "sig", 10);
        assert!(offer_from(&feed, "0.0.2", "x64").unwrap().is_none());
        assert!(offer_from(&feed, "0.0.3", "x64").unwrap().is_none());
        let offer = offer_from(&feed, "0.0.1", "x64").unwrap().unwrap();
        assert_eq!(offer.file_name, "Rinflow-Setup-0.0.2-x64.exe");
        assert_eq!(offer.size, 10);
        assert!(offer_from(&feed, "0.0.1", "arm64").is_err());
        assert!(offer_from(b"not json", "0.0.1", "x64").is_err());

        // A URL that leads to another file than the one signed.
        let renamed = String::from_utf8(feed)
            .unwrap()
            .replace("Rinflow-Setup-0.0.2-x64.exe", "Other.exe");
        assert!(offer_from(renamed.as_bytes(), "0.0.1", "x64").is_err());
    }

    #[test]
    fn the_signature_must_name_the_file_and_version() {
        let ours = keys();
        let data = b"setup program";
        let name = "Rinflow-Setup-0.0.2-x64.exe";
        let good = sign(&ours, data, &format!("file:{name}\tversion:0.0.2"));
        let (key, signature) = signature_for(&good, &ours.public, name, "0.0.2").unwrap();
        assert!(key.verify(data, &signature, false).is_ok());
        // Signed for another version: an old setup offered as a new one.
        let old = sign(
            &ours,
            data,
            "file:Rinflow-Setup-0.0.1-x64.exe\tversion:0.0.1",
        );
        assert!(signature_for(&old, &ours.public, name, "0.0.2").is_none());
        // Made with another key.
        let (key, signature) = signature_for(&good, &keys().public, name, "0.0.2").unwrap();
        assert!(key.verify(data, &signature, false).is_err());
    }

    /// Serves `body` once on 127.0.0.1; the URL ends with `name`.
    fn serve(body: Vec<u8>, name: &str) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request);
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(head.as_bytes()).unwrap();
            stream.write_all(&body).unwrap();
        });
        format!("http://127.0.0.1:{port}/download/{name}")
    }

    fn offer(keys: &Keys, served: &[u8], signed: &[u8]) -> Offer {
        let name = "Rinflow-Setup-0.0.2-x64.exe";
        Offer {
            version: "0.0.2".to_owned(),
            notes_url: None,
            url: serve(served.to_vec(), name),
            size: served.len() as u64,
            signature: sign(keys, signed, &format!("file:{name}\tversion:0.0.2")),
            file_name: name.to_owned(),
        }
    }

    fn directory(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("rinflow-update-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        directory
    }

    #[test]
    fn a_download_is_kept_only_when_signed() {
        let keys = keys();
        let setup: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();

        let folder = directory("good");
        let mut seen = 0.0;
        let good = offer(&keys, &setup, &setup);
        let path = download(&good, &folder, &keys.public, |p| seen = p).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), setup);
        assert_eq!(seen, 1.0);
        assert!(verify_file(&path, &good.signature, &keys.public).is_ok());
        // Changed on disk afterwards.
        std::fs::write(&path, b"something else").unwrap();
        assert!(verify_file(&path, &good.signature, &keys.public).is_err());

        let folder = directory("tampered");
        let mut tampered = setup.clone();
        tampered[1000] ^= 1;
        let result = download(
            &offer(&keys, &tampered, &setup),
            &folder,
            &keys.public,
            |_| {},
        );
        assert!(matches!(result, Err(DownloadError::Signature)));
        assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 0);
    }
}
