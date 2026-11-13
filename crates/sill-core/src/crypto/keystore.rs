//! Where this device's secrets are kept. The trait is deliberately small so that an OS keychain
//! backend can be added beside the file one.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Named secret bytes: load, save, delete.
pub trait KeyStore: Send + Sync {
    /// The secret called `name`, or `None` when there is none.
    fn load(&self, name: &str) -> io::Result<Option<Vec<u8>>>;
    /// Stores `secret` under `name`, replacing what was there.
    fn save(&self, name: &str, secret: &[u8]) -> io::Result<()>;
    /// Removes `name`; not an error when it is not there.
    fn delete(&self, name: &str) -> io::Result<()>;
}

/// Secrets as files in a directory, each readable by its owner alone (mode 0600 on Unix; on
/// Windows the directory's own ACL, a per-user profile's, is what protects it).
#[derive(Debug, Clone)]
pub struct FileKeyStore {
    dir: PathBuf,
}

impl FileKeyStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, name: &str) -> io::Result<PathBuf> {
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.') || name.starts_with('.') {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a secret's name"));
        }
        Ok(self.dir.join(format!("{name}.key")))
    }
}

impl KeyStore for FileKeyStore {
    fn load(&self, name: &str) -> io::Result<Option<Vec<u8>>> {
        match fs::read(self.path(name)?) {
            Ok(d) => Ok(Some(d)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn save(&self, name: &str, secret: &[u8]) -> io::Result<()> {
        let path = self.path(name)?;
        fs::create_dir_all(&self.dir)?;
        write_private(&path, secret)
    }

    fn delete(&self, name: &str) -> io::Result<()> {
        match fs::remove_file(self.path(name)?) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

/// Writes `data` to `path` through a temporary file beside it, so a crash never leaves half a
/// file, readable by the owner alone where the system has such modes.
pub(crate) fn write_private(path: &Path, data: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    {
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut f = options.open(&tmp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        f.write_all(data)?;
        f.sync_all()?;
    }
    // Windows' rename does not replace an existing file.
    #[cfg(windows)]
    {
        let _ = fs::remove_file(path);
    }
    fs::rename(&tmp, path)
}

/// A store that lives in memory, for tests.
#[derive(Default)]
pub struct MemoryKeyStore {
    items: std::sync::Mutex<std::collections::HashMap<String, Vec<u8>>>,
}

impl KeyStore for MemoryKeyStore {
    fn load(&self, name: &str) -> io::Result<Option<Vec<u8>>> {
        Ok(self.items.lock().unwrap().get(name).cloned())
    }
    fn save(&self, name: &str, secret: &[u8]) -> io::Result<()> {
        self.items.lock().unwrap().insert(name.to_string(), secret.to_vec());
        Ok(())
    }
    fn delete(&self, name: &str) -> io::Result<()> {
        self.items.lock().unwrap().remove(name);
        Ok(())
    }
}
