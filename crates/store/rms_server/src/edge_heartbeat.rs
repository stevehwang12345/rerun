//! Runtime-only replay protection for signed RMS Edge Agent heartbeats.

use std::{
    collections::BTreeMap,
    io::{self, Read as _},
    path::Path,
    sync::Arc,
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;

use tokio::task::AbortHandle;

const MAX_TOKEN_BYTES: usize = 512;
const MIN_TOKEN_BYTES: usize = 32;

#[derive(Default)]
pub(crate) struct EdgeHeartbeatRuntime {
    pub(crate) nonces: BTreeMap<String, i64>,
    pub(crate) sequences: BTreeMap<(String, String), (u64, i64)>,
    pub(crate) deadlines: BTreeMap<String, i64>,
    pub(crate) stale_tasks: BTreeMap<String, AbortHandle>,
}

pub(crate) fn fixture_token() -> Arc<Vec<u8>> {
    Arc::new(b"rms-edge-fixture-bearer-token-v1".to_vec())
}

pub(crate) fn load_token_from_environment() -> io::Result<Option<Arc<Vec<u8>>>> {
    let Some(path) = std::env::var_os("RMS_EDGE_BEARER_TOKEN_FILE") else {
        return Ok(None);
    };
    load_token(Path::new(&path)).map(Some)
}

#[expect(
    clippy::verbose_file_reads,
    reason = "the opened handle is reused for metadata validation to avoid a path race"
)]
fn load_token(path: &Path) -> io::Result<Arc<Vec<u8>>> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "RMS Edge bearer token path must be absolute",
        ));
    }
    let mut file = std::fs::File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "RMS Edge bearer token must be stored in a regular file",
        ));
    }
    #[cfg(unix)]
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "RMS Edge bearer token file must not grant group or world permissions",
        ));
    }
    // Portable Rust does not expose Windows ACLs. Windows deployments must restrict this file to
    // the RMS service identity through their installer or secret-provisioning system.
    if metadata.len() > (MAX_TOKEN_BYTES + 16) as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "RMS Edge bearer token file is too large",
        ));
    }
    let mut token = Vec::with_capacity(metadata.len() as usize);
    file.read_to_end(&mut token)?;
    while token.last().is_some_and(u8::is_ascii_whitespace) {
        token.pop();
    }
    if !(MIN_TOKEN_BYTES..=MAX_TOKEN_BYTES).contains(&token.len())
        || !token.iter().all(u8::is_ascii_graphic)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "RMS Edge bearer token must contain 32 to 512 visible ASCII bytes",
        ));
    }
    Ok(Arc::new(token))
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    #[test]
    fn token_loader_requires_an_absolute_regular_file_and_valid_length() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            load_token(Path::new("relative-token")).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(load_token(root.path()).is_err());

        let path = root.path().join("token");
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(b"edge-heartbeat-token-that-is-at-least-32-bytes\n")
            .unwrap();
        drop(file);
        #[cfg(unix)]
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            load_token(&path).unwrap().as_slice(),
            b"edge-heartbeat-token-that-is-at-least-32-bytes"
        );

        std::fs::write(&path, b"short").unwrap();
        assert_eq!(
            load_token(&path).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[cfg(unix)]
    #[test]
    fn token_loader_rejects_group_or_world_permissions() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("token");
        std::fs::write(&path, b"edge-heartbeat-token-that-is-at-least-32-bytes").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert_eq!(
            load_token(&path).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    }
}
