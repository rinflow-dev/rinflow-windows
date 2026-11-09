// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Signs Rinflow's setup programs for the agent's updater
//! (agent/src/update.rs), in minisign's format.
//!
//! ```text
//! update-signing generate <secret key file>
//!     Makes a key pair. The secret key goes to the file (never into the
//!     repository: it belongs in the RINFLOW_UPDATE_KEY secret of the release
//!     workflow), the public key is printed for update.rs.
//!
//! update-signing sign --version <version> <setup.exe>...
//!     Writes <setup.exe>.minisig for each, with the secret key from the
//!     RINFLOW_UPDATE_KEY environment variable. The signed (trusted) comment
//!     names the file and the version, so a signature cannot vouch for
//!     another file, or for an older version offered as a newer one.
//!
//! update-signing manifest --version <version> --base-url <url> --notes-url <url>
//!                         --out <latest.json> <setup.exe>...
//!     Writes the feed the updater reads: per architecture the download URL,
//!     the size and the signature (the .minisig beside each file).
//!
//! update-signing verify --public-key <base64> <setup.exe>
//!     Checks <setup.exe>.minisig as the updater does.
//! ```

use minisign::{KeyPair, PublicKey, SecretKeyBox, SignatureBox};
use serde_json::json;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let result = match arguments.first().map(String::as_str) {
        Some("generate") => generate(&arguments[1..]),
        Some("sign") => sign(&arguments[1..]),
        Some("manifest") => manifest(&arguments[1..]),
        Some("verify") => verify(&arguments[1..]),
        _ => Err(
            "usage: update-signing generate | sign | manifest | verify (see src/main.rs)".into(),
        ),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("update-signing: {error}");
            ExitCode::FAILURE
        }
    }
}

type Result<T = ()> = std::result::Result<T, String>;

/// `--name value` options and the remaining arguments.
struct Options {
    named: Vec<(String, String)>,
    files: Vec<PathBuf>,
}

impl Options {
    fn parse(arguments: &[String]) -> Result<Self> {
        let mut named = Vec::new();
        let mut files = Vec::new();
        let mut rest = arguments.iter();
        while let Some(argument) = rest.next() {
            if let Some(name) = argument.strip_prefix("--") {
                let value = rest.next().ok_or(format!("--{name} needs a value"))?;
                named.push((name.to_owned(), value.clone()));
            } else {
                files.push(PathBuf::from(argument));
            }
        }
        Ok(Self { named, files })
    }

    fn get(&self, name: &str) -> Result<&str> {
        self.named
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
            .ok_or(format!("--{name} is missing"))
    }

    fn files(&self) -> Result<&[PathBuf]> {
        if self.files.is_empty() {
            return Err("no files given".into());
        }
        Ok(&self.files)
    }
}

fn file_name(path: &Path) -> Result<&str> {
    path.file_name()
        .and_then(|name| name.to_str())
        .ok_or(format!("{} has no file name", path.display()))
}

/// What the updater requires in the signed comment of a file.
fn trusted_comment(name: &str, version: &str) -> String {
    format!("file:{name}\tversion:{version}")
}

fn signature_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".minisig");
    PathBuf::from(name)
}

fn generate(arguments: &[String]) -> Result {
    let [path] = arguments else {
        return Err("usage: update-signing generate <secret key file>".into());
    };
    let path = Path::new(path);
    if path.exists() {
        return Err(format!("{} exists; not replacing a key", path.display()));
    }
    if let Some(folder) = path
        .parent()
        .filter(|folder| !folder.as_os_str().is_empty())
    {
        fs::create_dir_all(folder).map_err(|error| error.to_string())?;
    }
    let pair = KeyPair::generate_unencrypted_keypair().map_err(|error| error.to_string())?;
    let secret = pair
        .sk
        .to_box(Some("Rinflow update signing key"))
        .map_err(|error| error.to_string())?
        .into_string();
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    file.write_all(secret.as_bytes())
        .map_err(|error| error.to_string())?;
    println!("secret key: {}", path.display());
    println!("public key: {}", pair.pk.to_base64());
    Ok(())
}

fn sign(arguments: &[String]) -> Result {
    let options = Options::parse(arguments)?;
    let version = options.get("version")?;
    let secret = std::env::var("RINFLOW_UPDATE_KEY")
        .map_err(|_| "RINFLOW_UPDATE_KEY (the secret key) is not set".to_owned())?;
    let secret = SecretKeyBox::from_string(secret.trim())
        .and_then(SecretKeyBox::into_unencrypted_secret_key)
        .map_err(|error| format!("RINFLOW_UPDATE_KEY is not a usable key: {error}"))?;
    for path in options.files()? {
        let name = file_name(path)?;
        let data = fs::File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
        let signature = minisign::sign(
            None,
            &secret,
            data,
            Some(&trusted_comment(name, version)),
            Some("Rinflow update signature"),
        )
        .map_err(|error| error.to_string())?;
        let out = signature_path(path);
        fs::write(&out, signature.into_string()).map_err(|error| error.to_string())?;
        println!("signed {name}");
    }
    Ok(())
}

fn manifest(arguments: &[String]) -> Result {
    let options = Options::parse(arguments)?;
    let version = options.get("version")?;
    let base_url = options.get("base-url")?.trim_end_matches('/');
    let mut platforms = serde_json::Map::new();
    for path in options.files()? {
        let name = file_name(path)?;
        let arch = ["x64", "arm64"]
            .into_iter()
            .find(|arch| name.ends_with(&format!("-{arch}.exe")))
            .ok_or(format!(
                "{name}: not a -x64.exe or -arm64.exe setup program"
            ))?;
        let size = fs::metadata(path)
            .map_err(|error| format!("{}: {error}", path.display()))?
            .len();
        let signature = fs::read_to_string(signature_path(path))
            .map_err(|error| format!("{name}.minisig: {error} (run sign first)"))?;
        platforms.insert(
            arch.to_owned(),
            json!({ "url": format!("{base_url}/{name}"), "size": size, "signature": signature }),
        );
    }
    let feed = json!({
        "version": version,
        "notes_url": options.get("notes-url")?,
        "platforms": platforms,
    });
    let out = options.get("out")?;
    let text = serde_json::to_string_pretty(&feed).map_err(|error| error.to_string())?;
    fs::write(out, text).map_err(|error| format!("{out}: {error}"))?;
    println!("wrote {out}");
    Ok(())
}

fn verify(arguments: &[String]) -> Result {
    let options = Options::parse(arguments)?;
    let public_key = PublicKey::from_base64(options.get("public-key")?)
        .map_err(|error| format!("--public-key: {error}"))?;
    for path in options.files()? {
        let signature = SignatureBox::from_file(signature_path(path))
            .map_err(|error| format!("{}.minisig: {error}", path.display()))?;
        let data = fs::File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
        minisign::verify(&public_key, &signature, data, true, false, false)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        println!(
            "{}: good signature ({})",
            path.display(),
            signature.trusted_comment().unwrap_or_default()
        );
    }
    Ok(())
}
