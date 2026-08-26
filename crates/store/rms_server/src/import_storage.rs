use std::{
    collections::BTreeMap,
    io::{self, Write as _},
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use axum::{
    body::Body,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse as _, Response},
};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};
use tokio_util::io::ReaderStream;

use crate::domain::{
    DataAssignment, DataSource, Device, DeviceAssignment, EdgeEnrollment, Integration, Project,
    Recording, RecordingImport, RecordingProjectSnapshot, Topic,
};

pub(crate) const DEFAULT_MAX_UPLOAD_BYTES: u64 = 512 * 1024 * 1024;
pub(crate) const DEFAULT_STORAGE_QUOTA_BYTES: u64 = 20 * 1024 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StoredImportArtifact {
    pub source_relative_path: String,
    pub rrd_relative_path: Option<String>,
    pub source_content_type: String,
    pub internal_detail: Option<String>,
    pub organization_id: String,
    pub project_snapshot: RecordingProjectSnapshot,
    pub topic_ids: Vec<String>,
    pub mapping_version: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StoredImportReceipt {
    pub fingerprint: String,
    pub import_id: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DurableRegistry {
    #[serde(default)]
    pub schema_version: u32,
    pub snapshot_version: u64,
    #[serde(default)]
    pub integrations: BTreeMap<String, Integration>,
    #[serde(default)]
    pub devices: BTreeMap<String, Device>,
    #[serde(default)]
    pub data_sources: BTreeMap<String, DataSource>,
    #[serde(default)]
    pub projects: BTreeMap<String, Project>,
    #[serde(default)]
    pub device_assignments: BTreeMap<String, DeviceAssignment>,
    #[serde(default)]
    pub data_assignments: BTreeMap<String, DataAssignment>,
    #[serde(default)]
    pub topics_by_data_source: BTreeMap<String, Vec<Topic>>,
    #[serde(default)]
    pub topics_by_recording: BTreeMap<String, Vec<Topic>>,
    #[serde(default)]
    pub edge_enrollments: BTreeMap<String, EdgeEnrollment>,
    #[serde(default)]
    pub imports: BTreeMap<String, RecordingImport>,
    #[serde(default)]
    pub recordings: BTreeMap<String, Recording>,
    // Legacy v0 import-only fields retained for one-way migration.
    #[serde(default)]
    pub import_data_sources: BTreeMap<String, DataSource>,
    #[serde(default)]
    pub import_data_assignments: BTreeMap<String, DataAssignment>,
    #[serde(default)]
    pub import_artifacts: BTreeMap<String, StoredImportArtifact>,
    #[serde(default)]
    pub recording_artifacts: BTreeMap<String, String>,
    #[serde(default)]
    pub import_receipts: BTreeMap<String, StoredImportReceipt>,
}

#[derive(Clone)]
pub(crate) struct ImportStorage {
    root: Arc<PathBuf>,
    max_upload_bytes: u64,
    storage_quota_bytes: u64,
    used_bytes: Arc<AtomicU64>,
}

pub(crate) struct StagedImportDeletion {
    original_path: PathBuf,
    staged_path: PathBuf,
    bytes: u64,
    exists: bool,
}

impl ImportStorage {
    pub(crate) fn open(root: impl AsRef<Path>, max_upload_bytes: u64) -> io::Result<Self> {
        Self::open_with_limits(root, max_upload_bytes, DEFAULT_STORAGE_QUOTA_BYTES)
    }

    pub(crate) fn open_with_limits(
        root: impl AsRef<Path>,
        max_upload_bytes: u64,
        storage_quota_bytes: u64,
    ) -> io::Result<Self> {
        std::fs::create_dir_all(root.as_ref())?;
        let root = std::fs::canonicalize(root.as_ref())?;
        std::fs::create_dir_all(root.join("imports"))?;
        let trash = root.join("trash");
        std::fs::create_dir_all(&trash)?;
        let referenced_imports = match std::fs::read(root.join("registry.json")) {
            Ok(bytes) => {
                serde_json::from_slice::<DurableRegistry>(&bytes)
                    .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?
                    .imports
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => BTreeMap::new(),
            Err(err) => return Err(err),
        };
        for entry in std::fs::read_dir(&trash)? {
            let path = entry?.path();
            let import_id = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| io::Error::other("Trash entry is not valid UTF-8."))?;
            let original = root.join("imports").join(import_id);
            if referenced_imports.contains_key(import_id) && !original.exists() {
                std::fs::rename(path, original)?;
            } else if path.is_dir() {
                std::fs::remove_dir_all(path)?;
            } else {
                std::fs::remove_file(path)?;
            }
        }
        for entry in std::fs::read_dir(root.join("imports"))? {
            let path = entry?.path();
            let import_id = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| io::Error::other("Import entry is not valid UTF-8."))?;
            if !referenced_imports.contains_key(import_id) {
                let quarantined = trash.join(import_id);
                std::fs::rename(&path, &quarantined)?;
                if quarantined.is_dir() {
                    std::fs::remove_dir_all(quarantined)?;
                } else {
                    std::fs::remove_file(quarantined)?;
                }
            }
        }
        let used_bytes = directory_size(&root.join("imports"))?;
        Ok(Self {
            root: Arc::new(root),
            max_upload_bytes,
            storage_quota_bytes,
            used_bytes: Arc::new(AtomicU64::new(used_bytes)),
        })
    }

    pub(crate) fn from_environment() -> io::Result<Self> {
        let root = if let Some(root) = std::env::var_os("RMS_STORAGE_DIR") {
            PathBuf::from(root)
        } else {
            default_storage_root()?
        };
        let max_upload_bytes = std::env::var("RMS_MAX_UPLOAD_BYTES")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(DEFAULT_MAX_UPLOAD_BYTES);
        let storage_quota_bytes = std::env::var("RMS_STORAGE_QUOTA_BYTES")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(DEFAULT_STORAGE_QUOTA_BYTES);
        Self::open_with_limits(root, max_upload_bytes, storage_quota_bytes)
    }

    pub(crate) fn max_upload_bytes(&self) -> u64 {
        self.max_upload_bytes
    }

    pub(crate) fn storage_quota_bytes(&self) -> u64 {
        self.storage_quota_bytes
    }

    pub(crate) fn reserve_bytes(&self, additional_bytes: u64) -> bool {
        let mut used = self.used_bytes.load(Ordering::Acquire);
        loop {
            let Some(next) = used.checked_add(additional_bytes) else {
                return false;
            };
            if next > self.storage_quota_bytes {
                return false;
            }
            match self.used_bytes.compare_exchange_weak(
                used,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(observed) => used = observed,
            }
        }
    }

    pub(crate) fn release_bytes(&self, bytes: u64) {
        self.used_bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                Some(used.saturating_sub(bytes))
            })
            .expect("the quota release update is never rejected");
    }

    pub(crate) fn import_dir(&self, import_id: &str) -> PathBuf {
        self.root.join("imports").join(import_id)
    }

    pub(crate) fn relative_source_path(import_id: &str) -> String {
        format!("imports/{import_id}/source/upload.bin")
    }

    pub(crate) fn relative_rrd_path(import_id: &str) -> String {
        format!("imports/{import_id}/recording.rrd")
    }

    pub(crate) async fn create_import_dirs(&self, import_id: &str) -> io::Result<()> {
        tokio::fs::create_dir_all(self.import_dir(import_id).join("source")).await
    }

    pub(crate) fn resolve_for_write(&self, relative_path: &str) -> io::Result<PathBuf> {
        let relative = validate_relative_path(relative_path)?;
        Ok(self.root.join(relative))
    }

    pub(crate) async fn remove_import_dir(&self, import_id: &str) -> io::Result<()> {
        let staged = self.stage_import_deletion(import_id).await?;
        self.commit_import_deletion(staged).await
    }

    pub(crate) async fn stage_import_deletion(
        &self,
        import_id: &str,
    ) -> io::Result<StagedImportDeletion> {
        validate_import_id(import_id)?;
        let original_path = self.import_dir(import_id);
        let staged_path = self.root.join("trash").join(import_id);
        let bytes = tokio::task::spawn_blocking({
            let path = original_path.clone();
            move || directory_size(&path)
        })
        .await
        .map_err(io::Error::other)??;
        let exists = match tokio::fs::rename(&original_path, &staged_path).await {
            Ok(()) => true,
            Err(err) if err.kind() == io::ErrorKind::NotFound => false,
            Err(err) => return Err(err),
        };
        Ok(StagedImportDeletion {
            original_path,
            staged_path,
            bytes,
            exists,
        })
    }

    pub(crate) async fn rollback_import_deletion(
        &self,
        staged: StagedImportDeletion,
    ) -> io::Result<()> {
        if staged.exists {
            tokio::fs::rename(staged.staged_path, staged.original_path).await?;
        }
        Ok(())
    }

    pub(crate) async fn commit_import_deletion(
        &self,
        staged: StagedImportDeletion,
    ) -> io::Result<()> {
        if staged.exists {
            tokio::fs::remove_dir_all(staged.staged_path).await?;
            self.release_bytes(staged.bytes);
        }
        Ok(())
    }

    pub(crate) fn load_registry(&self) -> io::Result<DurableRegistry> {
        let path = self.root.join("registry.json");
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(DurableRegistry::default()),
            Err(err) => Err(err),
        }
    }

    pub(crate) async fn persist_registry(&self, registry: DurableRegistry) -> io::Result<()> {
        let storage = self.clone();
        tokio::task::spawn_blocking(move || storage.persist_registry_blocking(&registry))
            .await
            .map_err(io::Error::other)?
    }

    pub(crate) fn persist_registry_blocking(&self, registry: &DurableRegistry) -> io::Result<()> {
        let mut temp = tempfile::NamedTempFile::new_in(self.root.as_ref())?;
        serde_json::to_writer_pretty(&mut temp, registry)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        temp.write_all(b"\n")?;
        temp.as_file().sync_all()?;

        let destination = self.root.join("registry.json");
        let (file, source) = temp.keep().map_err(|err| err.error)?;
        drop(file);
        if let Err(err) = atomicwrites::replace_atomic(&source, &destination) {
            drop(std::fs::remove_file(source));
            return Err(err);
        }
        Ok(())
    }

    pub(crate) async fn file_response(
        &self,
        relative_path: &str,
        request_headers: &HeaderMap,
        content_type: &str,
        content_sha256: &str,
        immutable: bool,
    ) -> io::Result<Response> {
        let path = self.resolve_existing(relative_path).await?;
        let total_len = tokio::fs::metadata(&path).await?.len();
        let etag = format!("\"sha256:{content_sha256}\"");
        let range = request_headers.get(header::RANGE).and_then(|value| {
            let if_range_matches = request_headers
                .get(header::IF_RANGE)
                .is_none_or(|if_range| if_range.as_bytes() == etag.as_bytes());
            if if_range_matches {
                value
                    .to_str()
                    .ok()
                    .map(|value| parse_range(value, total_len))
            } else {
                None
            }
        });

        let (mut response, response_len) = match range {
            Some(Ok((start, end))) => {
                let mut file = tokio::fs::File::open(path).await?;
                file.seek(std::io::SeekFrom::Start(start)).await?;
                let response = Body::from_stream(ReaderStream::new(file.take(end - start + 1)))
                    .into_response();
                let mut response = response;
                *response.status_mut() = StatusCode::PARTIAL_CONTENT;
                response.headers_mut().insert(
                    header::CONTENT_RANGE,
                    HeaderValue::from_str(&format!("bytes {start}-{end}/{total_len}"))
                        .map_err(io::Error::other)?,
                );
                (response, end - start + 1)
            }
            Some(Err(())) => {
                let mut response = Body::empty().into_response();
                *response.status_mut() = StatusCode::RANGE_NOT_SATISFIABLE;
                response.headers_mut().insert(
                    header::CONTENT_RANGE,
                    HeaderValue::from_str(&format!("bytes */{total_len}"))
                        .map_err(io::Error::other)?,
                );
                (response, 0)
            }
            None => {
                let file = tokio::fs::File::open(path).await?;
                (
                    Body::from_stream(ReaderStream::new(file)).into_response(),
                    total_len,
                )
            }
        };

        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_str(content_type).map_err(io::Error::other)?,
        );
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            if immutable {
                HeaderValue::from_static("private, max-age=31536000, immutable")
            } else {
                HeaderValue::from_static("private, no-store")
            },
        );
        response
            .headers_mut()
            .insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
        response.headers_mut().insert(
            header::ETAG,
            HeaderValue::from_str(&etag).map_err(io::Error::other)?,
        );
        response.headers_mut().insert(
            header::CONTENT_LENGTH,
            HeaderValue::from_str(&response_len.to_string()).map_err(io::Error::other)?,
        );
        Ok(response)
    }

    async fn resolve_existing(&self, relative_path: &str) -> io::Result<PathBuf> {
        let joined = self.resolve_for_write(relative_path)?;
        let canonical = tokio::fs::canonicalize(joined).await?;
        if !canonical.starts_with(self.root.as_ref()) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Artifact path escapes the RMS storage root.",
            ));
        }
        Ok(canonical)
    }
}

fn default_storage_root() -> io::Result<PathBuf> {
    directories::ProjectDirs::from("io", "RMS", "RMS")
        .map(|directories| directories.data_local_dir().join("storage"))
        .ok_or_else(|| io::Error::other("The operating-system data directory is unavailable."))
}

fn directory_size(path: &Path) -> io::Result<u64> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(err) => return Err(err),
    };
    if metadata.is_file() {
        return Ok(metadata.len());
    }
    let mut total = 0_u64;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        total = total
            .checked_add(directory_size(&entry.path())?)
            .ok_or_else(|| io::Error::other("RMS storage usage overflowed u64."))?;
    }
    Ok(total)
}

fn validate_relative_path(path: &str) -> io::Result<&Path> {
    let path = Path::new(path);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Artifact path must contain normal relative components only.",
        ));
    }
    Ok(path)
}

fn validate_import_id(import_id: &str) -> io::Result<()> {
    if import_id.is_empty()
        || import_id.contains(['/', '\\'])
        || import_id == "."
        || import_id == ".."
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid import identifier.",
        ));
    }
    Ok(())
}

fn parse_range(value: &str, total_len: u64) -> Result<(u64, u64), ()> {
    let range = value.strip_prefix("bytes=").ok_or(())?;
    if total_len == 0 || range.contains(',') {
        return Err(());
    }
    let (start, end) = range.split_once('-').ok_or(())?;
    if start.is_empty() {
        let suffix_len = end.parse::<u64>().ok().ok_or(())?;
        if suffix_len == 0 {
            return Err(());
        }
        return Ok((total_len.saturating_sub(suffix_len), total_len - 1));
    }
    let start = start.parse::<u64>().ok().ok_or(())?;
    if start >= total_len {
        return Err(());
    }
    let end = if end.is_empty() {
        total_len - 1
    } else {
        end.parse::<u64>().ok().ok_or(())?.min(total_len - 1)
    };
    if end < start {
        return Err(());
    }
    Ok((start, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn import(id: &str) -> RecordingImport {
        RecordingImport {
            id: id.to_owned(),
            project_id: "project-logistics".to_owned(),
            device_id: "robot-07".to_owned(),
            data_source_id: "import-source".to_owned(),
            file_name: "capture.rrd".to_owned(),
            format: "rrd".to_owned(),
            status: "ready".to_owned(),
            progress_percent: 100,
            size_bytes: 4,
            source_sha256: Some("hash".to_owned()),
            artifact_url: None,
            recording_id: None,
            failure_reason: None,
            warnings: Vec::new(),
            created_at: "2026-08-20T00:00:00Z".to_owned(),
            updated_at: "2026-08-20T00:00:00Z".to_owned(),
            resource_version: 1,
        }
    }

    #[tokio::test]
    async fn registry_replace_and_delete_crash_windows_are_recovered() {
        let root = tempfile::tempdir().expect("temporary storage is available");
        let storage = ImportStorage::open(root.path(), 1024).expect("storage opens");
        let mut registry = DurableRegistry {
            snapshot_version: 1,
            ..DurableRegistry::default()
        };
        registry.imports.insert("keep".to_owned(), import("keep"));
        storage
            .persist_registry_blocking(&registry)
            .expect("first registry persists");
        registry.snapshot_version = 2;
        storage
            .persist_registry_blocking(&registry)
            .expect("registry atomically replaces its predecessor");
        assert_eq!(storage.load_registry().unwrap().snapshot_version, 2);

        tokio::fs::create_dir_all(storage.import_dir("keep").join("source"))
            .await
            .unwrap();
        tokio::fs::write(
            storage.import_dir("keep").join("source/upload.bin"),
            b"RRF2",
        )
        .await
        .unwrap();
        let _staged = storage
            .stage_import_deletion("keep")
            .await
            .expect("delete is staged");
        assert!(!storage.import_dir("keep").exists());

        let recovered = ImportStorage::open(root.path(), 1024).expect("storage restarts");
        assert!(recovered.import_dir("keep").exists());

        let _staged = recovered
            .stage_import_deletion("keep")
            .await
            .expect("delete is staged again");
        registry.imports.clear();
        recovered
            .persist_registry_blocking(&registry)
            .expect("deleted registry persists");
        let restarted = ImportStorage::open(root.path(), 1024).expect("storage restarts again");
        assert!(!restarted.import_dir("keep").exists());
        assert!(!root.path().join("trash/keep").exists());

        std::fs::create_dir_all(root.path().join("imports/orphan/source")).unwrap();
        std::fs::write(
            root.path().join("imports/orphan/source/upload.bin"),
            b"orphan",
        )
        .unwrap();
        let restarted = ImportStorage::open(root.path(), 1024).expect("orphan is scavenged");
        assert!(!restarted.import_dir("orphan").exists());
        assert!(restarted.reserve_bytes(1024));
    }
}
