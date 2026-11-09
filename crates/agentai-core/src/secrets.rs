//! API keys: in the macOS Keychain or the Windows Credential Manager; on Linux (and if the
//! system store fails) in a file only this user can read.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::Result;

#[cfg(any(target_os = "macos", windows))]
const SERVICE: &str = "Agent AI";

fn file() -> Option<PathBuf> {
    crate::store::data_dir().map(|d| d.join("keys.json"))
}

fn read_file() -> BTreeMap<String, String> {
    file().and_then(|f| std::fs::read_to_string(f).ok()).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn write_file(keys: &BTreeMap<String, String>) -> Result<()> {
    let Some(f) = file() else { return Ok(()) };
    crate::store::write_atomic(&f, serde_json::to_string_pretty(keys)?.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// The key saved for `provider`, or an empty string.
pub fn get(provider: &str) -> String {
    #[cfg(any(target_os = "macos", windows))]
    if let Ok(k) = keyring::Entry::new(SERVICE, provider).and_then(|e| e.get_password()) {
        return k;
    }
    read_file().get(provider).cloned().unwrap_or_default()
}

/// Save `key` for `provider` (an empty key removes it).
pub fn set(provider: &str, key: &str) -> Result<()> {
    let key = key.trim();
    #[cfg(any(target_os = "macos", windows))]
    {
        if let Ok(e) = keyring::Entry::new(SERVICE, provider) {
            let stored = if key.is_empty() {
                // Nothing saved is fine too.
                let _ = e.delete_credential();
                true
            } else {
                e.set_password(key).is_ok()
            };
            if stored {
                // Remove any copy left in the file from before.
                let mut keys = read_file();
                if keys.remove(provider).is_some() {
                    write_file(&keys)?;
                }
                return Ok(());
            }
        }
    }
    let mut keys = read_file();
    if key.is_empty() {
        keys.remove(provider);
    } else {
        keys.insert(provider.to_string(), key.to_string());
    }
    write_file(&keys)
}
