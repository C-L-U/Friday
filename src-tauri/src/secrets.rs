use anyhow::{ensure, Context};
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    path::{Path, PathBuf},
};
use zeroize::{Zeroize, ZeroizeOnDrop};

#[derive(Clone, Default, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct Keys {
    pub deepgram: String,
    pub xai: String,
}
impl Keys {
    pub fn ready(&self) -> bool {
        !self.deepgram.is_empty() && !self.xai.is_empty()
    }
}

pub fn deepgram_header(
    key: &str,
) -> Result<reqwest::header::HeaderValue, reqwest::header::InvalidHeaderValue> {
    let mut value = reqwest::header::HeaderValue::from_str(&format!("Token {key}"))?;
    value.set_sensitive(true);
    Ok(value)
}

fn entry(provider: &str) -> keyring::Result<keyring::Entry> {
    keyring::Entry::new("com.jarvis.voice", provider)
}

pub fn verified_write(entry: &keyring::Entry, value: &str) -> anyhow::Result<()> {
    entry.set_password(value).context("keyring write failed")?;
    let read = zeroize::Zeroizing::new(entry.get_password().context("keyring readback failed")?);
    ensure!(read.as_str() == value, "keyring readback mismatch");
    Ok(())
}

pub fn load(dir: &Path) -> anyhow::Result<Keys> {
    let path = dir.join("credentials.json");
    let fallback: Keys = if path.exists() {
        ensure!(
            !std::fs::symlink_metadata(&path)?.file_type().is_symlink(),
            "Credential file cannot be a symlink"
        );
        let encoded = zeroize::Zeroizing::new(std::fs::read(path)?);
        serde_json::from_slice(&encoded)?
    } else {
        Keys::default()
    };
    fn read(provider: &str) -> String {
        match entry(provider).and_then(|entry| entry.get_password()) {
            Ok(value) => {
                crate::diagnostics::record(
                    "secret_storage",
                    &format!(
                        "{provider}: keyring read; service=com.jarvis.voice, username={provider}"
                    ),
                    None,
                );
                Ok(value)
            }
            Err(keyring::Error::NoEntry) => String::new(),
            Err(error) => {
                crate::diagnostics::error("keyring_read", &format!("{provider}: {error}"), None);
                Err(error.into())
            }
        }
    }
    if !fallback.deepgram.is_empty() || !fallback.xai.is_empty() {
        crate::diagnostics::record(
            "secret_storage",
            "Usando archivo de credenciales restringido (fallback del llavero)",
            None,
        );
    }
    Ok(Keys {
        deepgram: if !fallback.deepgram.is_empty() {
            fallback.deepgram.clone()
        } else {
            read("deepgram")
        },
        xai: if !fallback.xai.is_empty() {
            fallback.xai.clone()
        } else {
            read("xai")
        },
    })
}

pub fn save(dir: PathBuf, keys: Keys) -> anyhow::Result<()> {
    let mut fallback = Keys::default();
    for (name, value, slot) in [
        ("deepgram", &keys.deepgram, &mut fallback.deepgram),
        ("xai", &keys.xai, &mut fallback.xai),
    ] {
        if let Err(error) = entry(name)
            .map_err(anyhow::Error::from)
            .and_then(|e| verified_write(&e, value))
        {
            crate::diagnostics::record(
                "secret_storage",
                &format!("{name}: archivo restringido; {error:#}"),
                None,
            );
            tracing::warn!(
                provider = name,
                "OS keychain unavailable; using restricted credential file"
            );
            *slot = value.clone();
        } else {
            crate::diagnostics::record("secret_storage", &format!("{name}: keyring write/readback verified; service=com.jarvis.voice, username={name}"), None);
        }
    }
    let path = dir.join("credentials.json");
    if fallback.deepgram.is_empty() && fallback.xai.is_empty() {
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        return Ok(());
    }
    if path.exists() {
        ensure!(
            !std::fs::symlink_metadata(&path)?.file_type().is_symlink(),
            "Credential file cannot be a symlink"
        );
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let who = std::process::Command::new("whoami")
            .creation_flags(0x08000000)
            .output()?;
        ensure!(who.status.success(), "Cannot determine credential owner");
        let account = String::from_utf8(who.stdout)?;
        let result = std::process::Command::new("icacls")
            .arg(&path)
            .args([
                "/inheritance:r",
                "/grant:r",
                &format!("{}:F", account.trim()),
            ])
            .creation_flags(0x08000000)
            .output()?;
        ensure!(
            result.status.success(),
            "Cannot restrict credential file access"
        );
    }
    let mut encoded = serde_json::to_vec(&fallback)?;
    let result = file.write_all(&encoded).context("Cannot save credentials");
    encoded.zeroize();
    result?;
    file.sync_all()?;
    let encoded = zeroize::Zeroizing::new(std::fs::read(&path)?);
    let read: Keys = serde_json::from_slice(&encoded)?;
    ensure!(
        read.deepgram == fallback.deepgram && read.xai == fallback.xai,
        "credential file readback mismatch"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "requires native OS credential store"]
    fn native_keyring_roundtrip() {
        let user = format!("roundtrip-{}-{}", std::process::id(), fastrand::u64(..));
        let entry = keyring::Entry::new("com.jarvis.voice.test", &user).unwrap();
        let result = super::verified_write(&entry, "synthetic-roundtrip");
        let fresh = keyring::Entry::new("com.jarvis.voice.test", &user)
            .unwrap()
            .get_password();
        let cleanup = entry.delete_credential();
        result.unwrap();
        assert_eq!(fresh.unwrap(), "synthetic-roundtrip");
        cleanup.unwrap();
    }
}
