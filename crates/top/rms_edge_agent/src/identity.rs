//! Durable Ed25519 device identity and signing helpers.

use std::{
    fmt, fs, io,
    path::{Path, PathBuf},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::{
    rand::SystemRandom,
    signature::{Ed25519KeyPair, KeyPair as _},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tempfile::NamedTempFile;
use uuid::Uuid;

const IDENTITY_SCHEMA_VERSION: u32 = 1;
const MAX_IDENTITY_BYTES: u64 = 16 * 1024;
const LOCK_ATTEMPTS: usize = 100;
const LOCK_RETRY: Duration = Duration::from_millis(20);

/// Public, safe-to-advertise identity.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicIdentity {
    /// Version-8 UUID deterministically bound to the public key.
    pub device_id: Uuid,
    /// Base64url-no-pad Ed25519 public key.
    pub public_key: String,
    /// Creation time in Unix milliseconds.
    pub created_at_ms: i64,
}

/// Stable private signing identity.
pub struct DeviceIdentity {
    public: PublicIdentity,
    key_pair: Ed25519KeyPair,
}

impl fmt::Debug for DeviceIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeviceIdentity")
            .field("public", &self.public)
            .field("key_pair", &"<redacted>")
            .finish()
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct StoredIdentity {
    schema_version: u32,
    device_id: Uuid,
    public_key: String,
    private_key_pkcs8: String,
    created_at_ms: i64,
}

impl DeviceIdentity {
    /// Load an existing identity or create exactly one under a bounded filesystem lock.
    pub fn load_or_create(path: &Path) -> Result<Self, IdentityError> {
        match Self::load(path) {
            Ok(identity) => return Ok(identity),
            Err(error) if error.kind != IdentityErrorKind::NotFound => return Err(error),
            Err(_) => {}
        }

        let parent = path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent).map_err(IdentityError::io)?;
        let lock_path = lock_path(path);
        let lock = acquire_lock(&lock_path)?;
        let result = match Self::load(path) {
            Ok(identity) => Ok(identity),
            Err(error) if error.kind == IdentityErrorKind::NotFound => {
                Self::generate_and_store(path)
            }
            Err(error) => Err(error),
        };
        drop(lock);
        result
    }

    /// Load and cryptographically validate an existing identity.
    pub fn load(path: &Path) -> Result<Self, IdentityError> {
        let metadata = match fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(IdentityError::not_found());
            }
            Err(error) => return Err(IdentityError::io(error)),
        };
        if metadata.len() > MAX_IDENTITY_BYTES {
            return Err(IdentityError::invalid("identity file exceeds size limit"));
        }
        #[cfg(unix)]
        validate_private_permissions(&metadata)?;
        let bytes = fs::read(path).map_err(IdentityError::io)?;
        let stored: StoredIdentity =
            serde_json::from_slice(&bytes).map_err(IdentityError::decode)?;
        if stored.schema_version != IDENTITY_SCHEMA_VERSION {
            return Err(IdentityError::invalid("unsupported identity schema"));
        }
        let private_key = URL_SAFE_NO_PAD
            .decode(&stored.private_key_pkcs8)
            .map_err(|_error| IdentityError::invalid("invalid private-key encoding"))?;
        let key_pair = Ed25519KeyPair::from_pkcs8(&private_key)
            .map_err(|_error| IdentityError::invalid("invalid Ed25519 private key"))?;
        let public_key = URL_SAFE_NO_PAD.encode(key_pair.public_key().as_ref());
        let device_id = device_id_from_public_key(key_pair.public_key().as_ref());
        if public_key != stored.public_key || device_id != stored.device_id {
            return Err(IdentityError::invalid(
                "identity public-key binding mismatch",
            ));
        }
        Ok(Self {
            public: PublicIdentity {
                device_id,
                public_key,
                created_at_ms: stored.created_at_ms,
            },
            key_pair,
        })
    }

    fn generate_and_store(path: &Path) -> Result<Self, IdentityError> {
        let random = SystemRandom::new();
        let document = Ed25519KeyPair::generate_pkcs8(&random)
            .map_err(|_error| IdentityError::invalid("failed to generate Ed25519 identity"))?;
        let key_pair = Ed25519KeyPair::from_pkcs8(document.as_ref())
            .map_err(|_error| IdentityError::invalid("failed to decode generated identity"))?;
        let public_key = URL_SAFE_NO_PAD.encode(key_pair.public_key().as_ref());
        let device_id = device_id_from_public_key(key_pair.public_key().as_ref());
        let created_at_ms = unix_time_ms()?;
        let stored = StoredIdentity {
            schema_version: IDENTITY_SCHEMA_VERSION,
            device_id,
            public_key: public_key.clone(),
            private_key_pkcs8: URL_SAFE_NO_PAD.encode(document.as_ref()),
            created_at_ms,
        };
        let encoded = serde_json::to_vec_pretty(&stored).map_err(IdentityError::encode)?;
        let parent = path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let mut temporary = NamedTempFile::new_in(parent).map_err(IdentityError::io)?;
        io::Write::write_all(&mut temporary, &encoded).map_err(IdentityError::io)?;
        io::Write::write_all(&mut temporary, b"\n").map_err(IdentityError::io)?;
        #[cfg(unix)]
        set_private_permissions(temporary.as_file())?;
        temporary.as_file().sync_all().map_err(IdentityError::io)?;
        let (_file, temporary_path) = temporary
            .keep()
            .map_err(|error| IdentityError::io(error.error))?;
        if let Err(error) = atomicwrites::replace_atomic(&temporary_path, path) {
            drop(fs::remove_file(&temporary_path));
            return Err(IdentityError::io(error));
        }
        Self::load(path)
    }

    /// Public identity fields.
    pub fn public(&self) -> &PublicIdentity {
        &self.public
    }

    /// Sign canonical bytes and return base64url-no-pad Ed25519 signature.
    pub fn sign_base64(&self, message: &[u8]) -> String {
        URL_SAFE_NO_PAD.encode(self.key_pair.sign(message).as_ref())
    }

    /// Verify the identity UUID binding without loading a private key.
    pub fn public_key_matches_device_id(
        public_key_base64: &str,
        device_id: Uuid,
    ) -> Result<bool, IdentityError> {
        let public_key = URL_SAFE_NO_PAD
            .decode(public_key_base64)
            .map_err(|_error| IdentityError::invalid("invalid public-key encoding"))?;
        if public_key.len() != 32 {
            return Err(IdentityError::invalid("invalid public-key length"));
        }
        Ok(device_id_from_public_key(&public_key) == device_id)
    }
}

/// Derive the RMS `UUIDv8` identity from an Ed25519 public key.
pub fn device_id_from_public_key(public_key: &[u8]) -> Uuid {
    let digest = Sha256::digest(public_key);
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IdentityErrorKind {
    NotFound,
    Other,
}

/// Durable identity error with no private key material.
#[derive(Debug)]
pub struct IdentityError {
    kind: IdentityErrorKind,
    message: String,
}

impl IdentityError {
    fn not_found() -> Self {
        Self {
            kind: IdentityErrorKind::NotFound,
            message: "identity file does not exist".to_owned(),
        }
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self {
            kind: IdentityErrorKind::Other,
            message: message.into(),
        }
    }

    fn io(error: impl fmt::Display) -> Self {
        Self::invalid(format!("identity I/O failed: {error}"))
    }

    fn decode(error: impl fmt::Display) -> Self {
        Self::invalid(format!("identity parsing failed: {error}"))
    }

    fn encode(error: impl fmt::Display) -> Self {
        Self::invalid(format!("identity encoding failed: {error}"))
    }
}

impl fmt::Display for IdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for IdentityError {}

struct IdentityLock {
    path: PathBuf,
    _file: fs::File,
}

impl Drop for IdentityLock {
    fn drop(&mut self) {
        drop(fs::remove_file(&self.path));
    }
}

fn acquire_lock(path: &Path) -> Result<IdentityLock, IdentityError> {
    for _ in 0..LOCK_ATTEMPTS {
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(file) => {
                return Ok(IdentityLock {
                    path: path.to_owned(),
                    _file: file,
                });
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                thread::sleep(LOCK_RETRY);
            }
            Err(error) => return Err(IdentityError::io(error)),
        }
    }
    Err(IdentityError::invalid(
        "identity initialization lock timed out",
    ))
}

fn lock_path(path: &Path) -> PathBuf {
    let mut value = path.as_os_str().to_owned();
    value.push(".lock");
    PathBuf::from(value)
}

fn unix_time_ms() -> Result<i64, IdentityError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_error| IdentityError::invalid("system clock precedes Unix epoch"))?;
    i64::try_from(duration.as_millis())
        .map_err(|_error| IdentityError::invalid("system clock exceeds supported range"))
}

#[cfg(unix)]
fn set_private_permissions(file: &fs::File) -> Result<(), IdentityError> {
    use std::os::unix::fs::PermissionsExt as _;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(IdentityError::io)
}

#[cfg(unix)]
fn validate_private_permissions(metadata: &fs::Metadata) -> Result<(), IdentityError> {
    use std::os::unix::fs::PermissionsExt as _;
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(IdentityError::invalid(
            "identity file permissions must not grant group/other access",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{ED25519, UnparsedPublicKey};

    #[test]
    fn identity_is_stable_and_signature_verifies() {
        let root = tempfile::tempdir().expect("temporary directory");
        let path = root.path().join("identity.json");
        let first = DeviceIdentity::load_or_create(&path).expect("identity is created");
        let signature = first.sign_base64(b"message");
        let second = DeviceIdentity::load_or_create(&path).expect("identity is reloaded");
        assert_eq!(first.public(), second.public());
        let public_key = URL_SAFE_NO_PAD
            .decode(&first.public().public_key)
            .expect("public key decodes");
        let signature = URL_SAFE_NO_PAD
            .decode(signature)
            .expect("signature decodes");
        UnparsedPublicKey::new(&ED25519, public_key)
            .verify(b"message", &signature)
            .expect("signature verifies");
    }

    #[test]
    fn uuid_is_version_eight_and_key_bound() {
        let public_key = [7_u8; 32];
        let device_id = device_id_from_public_key(&public_key);
        assert_eq!(device_id.get_version_num(), 8);
        assert!(
            DeviceIdentity::public_key_matches_device_id(
                &URL_SAFE_NO_PAD.encode(public_key),
                device_id
            )
            .expect("binding validates")
        );
    }
}
