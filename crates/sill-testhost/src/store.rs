//! The host's state on disk, when it is given a directory: `identity.json` (the private
//! key as PKCS#8 and the recognition key, both base64) and `paired.json` (the trust
//! list). This layout is this crate's own; the real host keeps both in the keychain
//! (`KeychainIdentityStore.swift`).

use crate::events::PairedDevice;
use crate::identity::{self, Identity};
use crate::HostConfig;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::io;
use std::path::Path;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredIdentity {
    pkcs8: String,
    recognition_key: String,
}

pub(crate) struct Stored {
    pub identity: Identity,
    pub recognition_key: [u8; 32],
    pub paired: Vec<PairedDevice>,
}

fn invalid(e: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}

/// Writes a file only its owner can read (the private key lives in one).
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    std::fs::write(path, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// The identity and trust list of `dir`, made on first use; fresh and in memory without one.
pub(crate) fn load_or_create(dir: Option<&Path>) -> io::Result<Stored> {
    let Some(dir) = dir else {
        return Ok(Stored { identity: Identity::generate()?, recognition_key: identity::random32(), paired: Vec::new() });
    };
    std::fs::create_dir_all(dir)?;
    let id_path = dir.join("identity.json");
    let (identity, recognition_key) = if id_path.exists() {
        let stored: StoredIdentity = serde_json::from_slice(&std::fs::read(&id_path)?).map_err(invalid)?;
        let pkcs8 = STANDARD.decode(&stored.pkcs8).map_err(invalid)?;
        let key: [u8; 32] = STANDARD
            .decode(&stored.recognition_key)
            .map_err(invalid)?
            .try_into()
            .map_err(|_| invalid("the recognition key is not 32 bytes"))?;
        (Identity::from_pkcs8(&pkcs8)?, key)
    } else {
        let identity = Identity::generate()?;
        let key = identity::random32();
        let stored = StoredIdentity { pkcs8: STANDARD.encode(&identity.pkcs8), recognition_key: STANDARD.encode(key) };
        write_private(&id_path, &serde_json::to_vec_pretty(&stored).map_err(invalid)?)?;
        (identity, key)
    };
    let paired_path = dir.join("paired.json");
    let paired = if paired_path.exists() {
        serde_json::from_slice(&std::fs::read(&paired_path)?).map_err(invalid)?
    } else {
        Vec::new()
    };
    Ok(Stored { identity, recognition_key, paired })
}

/// Saves the trust list when the host has a directory. A failure is printed, never fatal.
pub(crate) fn save_paired(cfg: &HostConfig, paired: &[PairedDevice]) {
    let Some(dir) = &cfg.dir else { return };
    let result = serde_json::to_vec_pretty(paired).map_err(invalid).and_then(|b| write_private(&dir.join("paired.json"), &b));
    if let Err(e) = result {
        eprintln!("sill-testhost: the paired devices could not be saved ({e}).");
    }
}
