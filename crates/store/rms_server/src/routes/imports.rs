use std::{collections::BTreeSet, path::Path, sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Multipart, Path as AxumPath, Query, State, multipart::Field},
    http::{HeaderMap, StatusCode, header},
    routing::get,
};
use rms_import::{ImportCancellation, ImportFormat, ImportRequest, ImportResult, TimelineKind};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use tokio::io::AsyncWriteExt as _;
use tokio::sync::OwnedSemaphorePermit;
use tokio::time::Instant;

use crate::{
    AppState,
    domain::{
        DataAssignment, DataSource, DataVisibility, Recording, RecordingImport,
        RecordingProjectSnapshot, TimelineDescriptor,
    },
    error::{ApiError, ApiResult},
    import_storage::{DEFAULT_MAX_UPLOAD_BYTES, StoredImportArtifact, StoredImportReceipt},
    state::{new_id, now_iso, topics_for_entity_descriptors, topics_for_source},
};

const MAX_TEXT_FIELD_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy)]
struct UploadDeadline {
    overall: Instant,
    idle_timeout: Duration,
}

impl UploadDeadline {
    fn from_environment() -> Self {
        let total = configured_timeout("RMS_UPLOAD_TOTAL_TIMEOUT_SECS", 15 * 60);
        let idle = configured_timeout("RMS_UPLOAD_IDLE_TIMEOUT_SECS", 30);
        Self {
            overall: Instant::now() + total,
            idle_timeout: idle,
        }
    }

    fn next_deadline(self) -> Instant {
        self.overall.min(Instant::now() + self.idle_timeout)
    }

    async fn next_field(self, multipart: &mut Multipart) -> ApiResult<Option<Field<'_>>> {
        tokio::time::timeout_at(self.next_deadline(), multipart.next_field())
            .await
            .map_err(|_elapsed| ApiError::request_timeout("The multipart upload timed out."))?
            .map_err(|_multipart| ApiError::bad_request("The multipart upload is incomplete."))
    }

    async fn next_chunk(self, field: &mut Field<'_>) -> ApiResult<Option<axum::body::Bytes>> {
        tokio::time::timeout_at(self.next_deadline(), field.chunk())
            .await
            .map_err(|_elapsed| ApiError::request_timeout("The multipart upload timed out."))?
            .map_err(|_multipart| ApiError::bad_request("The multipart upload is incomplete."))
    }
}

fn configured_timeout(variable: &str, default_seconds: u64) -> Duration {
    Duration::from_secs(
        std::env::var(variable)
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(default_seconds),
    )
}

fn parse_idempotency_key(headers: &HeaderMap) -> ApiResult<Option<String>> {
    let Some(value) = headers.get("idempotency-key") else {
        return Ok(None);
    };
    let key = value
        .to_str()
        .map_err(|_invalid_header| ApiError::bad_request("Idempotency-Key must be valid ASCII."))?
        .trim();
    if key.is_empty() || key.len() > 128 || key.chars().any(char::is_control) {
        return Err(ApiError::bad_request(
            "Idempotency-Key must contain 1 to 128 printable characters.",
        ));
    }
    Ok(Some(key.to_owned()))
}

fn import_fingerprint(
    project_id: &str,
    device_id: &str,
    data_source_id: Option<&str>,
    format: &str,
    mapping: Option<&serde_json::Value>,
    source_sha256: &str,
) -> String {
    let mut hasher = Sha256::new();
    for value in [
        project_id,
        device_id,
        data_source_id.unwrap_or_default(),
        format,
        source_sha256,
    ] {
        hasher.update(value.as_bytes());
        hasher.update([0]);
    }
    if let Some(mapping) = mapping {
        hasher.update(
            serde_json::to_vec(mapping).expect("a parsed JSON mapping is always serializable"),
        );
    }
    format!("{:x}", hasher.finalize())
}

async fn idempotent_import(
    state: &AppState,
    idempotency_key: Option<&str>,
    fingerprint: &str,
) -> ApiResult<Option<RecordingImport>> {
    let Some(key) = idempotency_key else {
        return Ok(None);
    };
    let catalog = state.catalog.read().await;
    let Some(receipt) = catalog.import_receipts.get(key) else {
        return Ok(None);
    };
    if receipt.fingerprint != fingerprint {
        return Err(ApiError::conflict(
            "Idempotency-Key was already used for a different recording import.",
        ));
    }
    catalog
        .recording_imports
        .get(&receipt.import_id)
        .cloned()
        .map(Some)
        .ok_or_else(|| ApiError::internal("Import idempotency receipt is inconsistent."))
}

async fn acquire_import_permit(
    state: &AppState,
    upload_deadline: UploadDeadline,
) -> ApiResult<OwnedSemaphorePermit> {
    tokio::time::timeout_at(
        upload_deadline.overall,
        state.import_slots.clone().acquire_owned(),
    )
    .await
    .map_err(|_elapsed| ApiError::request_timeout("Import admission timed out."))?
    .map_err(|_closed| ApiError::internal("Import admission is unavailable."))
}

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/recording-imports",
            get(list_imports).post(create_import),
        )
        .route(
            "/api/v1/recording-imports/{import_id}",
            get(get_import).delete(delete_import),
        )
        .route(
            "/api/v1/recording-imports/{import_id}/artifact",
            get(open_source_artifact),
        )
        // The extractor-level cap bounds malformed or unterminated multipart headers before a
        // file field exists. The streaming quota applies the configured, potentially lower cap.
        .layer(DefaultBodyLimit::max(
            DEFAULT_MAX_UPLOAD_BYTES as usize + 1024 * 1024,
        ))
}

#[derive(Default)]
struct UploadFields {
    project_id: Option<String>,
    device_id: Option<String>,
    data_source_id: Option<String>,
    requested_format: Option<String>,
    mapping: Option<serde_json::Value>,
    file: Option<UploadedFile>,
}

struct UploadedFile {
    file_name: String,
    relative_path: String,
    format: ImportFormat,
    format_label: String,
    content_type: String,
    size_bytes: u64,
    sha256: String,
}

struct ImportDirGuard {
    path: std::path::PathBuf,
    storage: Arc<crate::import_storage::ImportStorage>,
    reserved_bytes: u64,
    armed: bool,
}

impl ImportDirGuard {
    fn new(path: std::path::PathBuf, storage: Arc<crate::import_storage::ImportStorage>) -> Self {
        Self {
            path,
            storage,
            reserved_bytes: 0,
            armed: true,
        }
    }

    fn reserve(&mut self, bytes: u64) -> ApiResult<()> {
        if !self.storage.reserve_bytes(bytes) {
            return Err(ApiError::payload_too_large(format!(
                "RMS storage has reached its {} byte quota.",
                self.storage.storage_quota_bytes()
            )));
        }
        self.reserved_bytes = self.reserved_bytes.saturating_add(bytes);
        Ok(())
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ImportDirGuard {
    fn drop(&mut self) {
        if self.armed {
            drop(std::fs::remove_dir_all(&self.path));
            self.storage.release_bytes(self.reserved_bytes);
        }
    }
}

struct OutputReservation {
    storage: Arc<crate::import_storage::ImportStorage>,
    reserved_bytes: u64,
}

impl OutputReservation {
    fn reserve(storage: Arc<crate::import_storage::ImportStorage>, bytes: u64) -> ApiResult<Self> {
        if !storage.reserve_bytes(bytes) {
            return Err(ApiError::payload_too_large(format!(
                "RMS storage has reached its {} byte quota.",
                storage.storage_quota_bytes()
            )));
        }
        Ok(Self {
            storage,
            reserved_bytes: bytes,
        })
    }

    fn keep_actual(&mut self, actual_bytes: u64) {
        self.storage
            .release_bytes(self.reserved_bytes.saturating_sub(actual_bytes));
        self.reserved_bytes = 0;
    }
}

impl Drop for OutputReservation {
    fn drop(&mut self) {
        self.storage.release_bytes(self.reserved_bytes);
    }
}

async fn create_import(
    State(state): State<AppState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> ApiResult<(StatusCode, Json<RecordingImport>)> {
    let idempotency_key = parse_idempotency_key(&headers)?;
    if headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|content_length| {
            content_length
                > state
                    .import_storage
                    .max_upload_bytes()
                    .saturating_add(1024 * 1024)
        })
    {
        return Err(ApiError::payload_too_large(
            "The multipart request exceeds the upload limit.",
        ));
    }
    let upload_deadline = UploadDeadline::from_environment();
    let _upload_permit = tokio::time::timeout_at(
        upload_deadline.overall,
        state.upload_slots.clone().acquire_owned(),
    )
    .await
    .map_err(|_elapsed| ApiError::request_timeout("Upload admission timed out."))?
    .map_err(|_closed| ApiError::internal("Upload admission is unavailable."))?;
    let import_id = new_id("recording-import");
    state
        .import_storage
        .create_import_dirs(&import_id)
        .await
        .map_err(|_io| ApiError::internal("Import storage could not be prepared."))?;
    let mut cleanup = ImportDirGuard::new(
        state.import_storage.import_dir(&import_id),
        Arc::clone(&state.import_storage),
    );

    let mut fields = UploadFields::default();
    while let Some(field) = upload_deadline.next_field(&mut multipart).await? {
        let field_name = field
            .name()
            .ok_or_else(|| ApiError::bad_request("Every multipart field must have a name."))?
            .to_owned();
        if fields.file.is_some() {
            return Err(ApiError::bad_request(
                "file must be the final multipart field.",
            ));
        }
        match field_name.as_str() {
            "file" => {
                let project_id = required(fields.project_id.clone(), "projectId")?;
                let device_id = required(fields.device_id.clone(), "deviceId")?;
                let requested_format = required(fields.requested_format.clone(), "format")?;
                validate_import_scope(
                    &state,
                    &project_id,
                    &device_id,
                    fields.data_source_id.as_deref(),
                )
                .await?;
                fields.file = Some(
                    write_upload(
                        &state,
                        &import_id,
                        field,
                        &requested_format,
                        &mut cleanup,
                        upload_deadline,
                    )
                    .await?,
                );
            }
            "projectId" => {
                set_once(
                    &mut fields.project_id,
                    read_text(field, MAX_TEXT_FIELD_BYTES, upload_deadline).await?,
                    "projectId",
                )?;
            }
            "deviceId" => {
                set_once(
                    &mut fields.device_id,
                    read_text(field, MAX_TEXT_FIELD_BYTES, upload_deadline).await?,
                    "deviceId",
                )?;
            }
            "dataSourceId" => {
                set_once(
                    &mut fields.data_source_id,
                    read_text(field, MAX_TEXT_FIELD_BYTES, upload_deadline).await?,
                    "dataSourceId",
                )?;
            }
            "format" => {
                set_once(
                    &mut fields.requested_format,
                    read_text(field, MAX_TEXT_FIELD_BYTES, upload_deadline).await?,
                    "format",
                )?;
            }
            "mapping" => {
                if fields.mapping.is_some() {
                    return Err(ApiError::bad_request("mapping must not be repeated."));
                }
                let value = read_text(field, MAX_TEXT_FIELD_BYTES, upload_deadline).await?;
                let mapping =
                    serde_json::from_str::<serde_json::Value>(&value).map_err(|_json| {
                        ApiError::bad_request("mapping must be a valid JSON object.")
                    })?;
                if !mapping.is_object() {
                    return Err(ApiError::bad_request(
                        "mapping must be a valid JSON object.",
                    ));
                }
                fields.mapping = Some(mapping);
            }
            _ => {
                return Err(ApiError::bad_request(format!(
                    "Unknown multipart field `{field_name}`."
                )));
            }
        }
    }

    let project_id = required(fields.project_id, "projectId")?;
    let device_id = required(fields.device_id, "deviceId")?;
    let file = fields
        .file
        .ok_or_else(|| ApiError::bad_request("file is required."))?;
    let requested = required(fields.requested_format, "format")?;
    if requested != file.format_label {
        return Err(ApiError::unsupported_media_type(format!(
            "The selected format `{requested}` does not match the uploaded file."
        )));
    }
    if fields.mapping.is_some() && file.format != ImportFormat::Csv {
        return Err(ApiError::bad_request(
            "mapping is only accepted for CSV imports.",
        ));
    }
    let fingerprint = import_fingerprint(
        &project_id,
        &device_id,
        fields.data_source_id.as_deref(),
        &file.format_label,
        fields.mapping.as_ref(),
        &file.sha256,
    );
    if let Some(existing) =
        idempotent_import(&state, idempotency_key.as_deref(), &fingerprint).await?
    {
        return Ok((StatusCode::ACCEPTED, Json(existing)));
    }
    let import_permit = acquire_import_permit(&state, upload_deadline).await?;
    let persist_guard = state.registry_persist_lock.lock().await;
    let previous_catalog = state.catalog.read().await.clone();
    if let Some(existing) =
        idempotent_import(&state, idempotency_key.as_deref(), &fingerprint).await?
    {
        return Ok((StatusCode::ACCEPTED, Json(existing)));
    }
    let assignment = prepare_import_assignment(
        &state,
        &project_id,
        &device_id,
        fields.data_source_id.as_deref(),
    )
    .await?;
    let created_at = now_iso();
    let source_artifact = StoredImportArtifact {
        source_relative_path: file.relative_path.clone(),
        rrd_relative_path: None,
        source_content_type: file.content_type.clone(),
        internal_detail: None,
        organization_id: assignment.organization_id,
        project_snapshot: assignment.project_snapshot,
        topic_ids: assignment.topic_ids,
        mapping_version: assignment.mapping_version,
    };
    let cancellation = ImportCancellation::new();
    let import = {
        let mut catalog = state.catalog.write().await;
        let resource_version = catalog.bump_version();
        let import = RecordingImport {
            id: import_id.clone(),
            project_id,
            device_id,
            data_source_id: assignment.data_source_id,
            file_name: file.file_name.clone(),
            format: file.format_label.clone(),
            status: "processing".to_owned(),
            progress_percent: 10,
            size_bytes: file.size_bytes,
            source_sha256: Some(file.sha256.clone()),
            artifact_url: Some(format!("/api/v1/recording-imports/{import_id}/artifact")),
            recording_id: None,
            failure_reason: None,
            warnings: Vec::new(),
            created_at: created_at.clone(),
            updated_at: created_at,
            resource_version,
        };
        catalog
            .recording_imports
            .insert(import_id.clone(), import.clone());
        catalog
            .import_artifacts
            .insert(import_id.clone(), source_artifact);
        catalog
            .import_cancellations
            .insert(import_id.clone(), cancellation.clone());
        if let Some(key) = idempotency_key {
            catalog.import_receipts.insert(
                key,
                StoredImportReceipt {
                    fingerprint,
                    import_id: import_id.clone(),
                },
            );
        }
        import
    };
    let registry = state.catalog.read().await.durable_registry();
    if state
        .import_storage
        .persist_registry(registry)
        .await
        .is_err()
    {
        let mut catalog = state.catalog.write().await;
        *catalog = previous_catalog;
        catalog
            .workspace_version
            .send_replace(catalog.snapshot_version);
        return Err(ApiError::internal(
            "Import metadata could not be persisted.",
        ));
    }
    drop(persist_guard);
    cleanup.disarm();

    spawn_import_worker(
        state,
        import.clone(),
        file,
        fields.mapping,
        import.resource_version,
        cancellation,
        import_permit,
    );
    Ok((StatusCode::ACCEPTED, Json(import)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListImportsQuery {
    project_id: Option<String>,
}

async fn list_imports(
    State(state): State<AppState>,
    Query(query): Query<ListImportsQuery>,
) -> Json<Vec<RecordingImport>> {
    let catalog = state.catalog.read().await;
    Json(
        catalog
            .recording_imports
            .values()
            .filter(|import| {
                query
                    .project_id
                    .as_ref()
                    .is_none_or(|project_id| import.project_id == *project_id)
            })
            .cloned()
            .collect(),
    )
}

async fn get_import(
    State(state): State<AppState>,
    AxumPath(import_id): AxumPath<String>,
) -> ApiResult<Json<RecordingImport>> {
    let catalog = state.catalog.read().await;
    catalog
        .recording_imports
        .get(&import_id)
        .cloned()
        .map(Json)
        .ok_or_else(|| ApiError::not_found("RecordingImport", &import_id))
}

async fn delete_import(
    State(state): State<AppState>,
    AxumPath(import_id): AxumPath<String>,
) -> ApiResult<StatusCode> {
    let cancelled = {
        let _persist_guard = state.registry_persist_lock.lock().await;
        let mut catalog = state.catalog.write().await;
        let import = catalog
            .recording_imports
            .get(&import_id)
            .cloned()
            .ok_or_else(|| ApiError::not_found("RecordingImport", &import_id))?;

        if let Some(recording_id) = import.recording_id.as_ref() {
            let replay_is_open = catalog
                .replay_sessions
                .values()
                .any(|session| session.recording_id == *recording_id && session.status == "open");
            if replay_is_open {
                return Err(ApiError::conflict(
                    "Close the active ReplaySession before deleting this import.",
                ));
            }
        }

        if matches!(import.status.as_str(), "uploading" | "processing") {
            let previous_import = import;
            let previous_snapshot_version = catalog.snapshot_version;
            let resource_version = catalog.bump_version();
            let current = catalog
                .recording_imports
                .get_mut(&import_id)
                .expect("import existence was checked");
            current.status = "cancelled".to_owned();
            current.updated_at = now_iso();
            current.resource_version = resource_version;
            let registry = catalog.durable_registry();
            if state
                .import_storage
                .persist_registry(registry)
                .await
                .is_err()
            {
                catalog
                    .recording_imports
                    .insert(import_id.clone(), previous_import);
                catalog.snapshot_version = previous_snapshot_version;
                catalog
                    .workspace_version
                    .send_replace(previous_snapshot_version);
                return Err(ApiError::internal(
                    "Import cancellation could not be persisted.",
                ));
            }
            if let Some(cancel) = catalog.import_cancellations.get(&import_id) {
                cancel.cancel();
            }
            true
        } else {
            if catalog.import_cancellations.contains_key(&import_id) {
                return Err(ApiError::conflict(
                    "The import worker is still finalizing cancellation. Try again shortly.",
                ));
            }
            false
        }
    };
    if cancelled {
        return Ok(StatusCode::NO_CONTENT);
    }

    // Completed imports use a same-volume trash rename before the registry transaction. This
    // makes either the old catalog+directory or the new catalog+trash recoverable after a crash.
    let _persist_guard = state.registry_persist_lock.lock().await;
    let mut catalog = state.catalog.write().await;
    let import = catalog
        .recording_imports
        .get(&import_id)
        .cloned()
        .ok_or_else(|| ApiError::not_found("RecordingImport", &import_id))?;
    if catalog.import_cancellations.contains_key(&import_id) {
        return Err(ApiError::conflict(
            "The import worker is still finalizing. Try again shortly.",
        ));
    }
    if let Some(recording_id) = import.recording_id.as_ref() {
        let replay_is_open = catalog
            .replay_sessions
            .values()
            .any(|session| session.recording_id == *recording_id && session.status == "open");
        if replay_is_open {
            return Err(ApiError::conflict(
                "Close the active ReplaySession before deleting this import.",
            ));
        }
    }
    let staged = state
        .import_storage
        .stage_import_deletion(&import_id)
        .await
        .map_err(|_io| ApiError::internal("Import artifacts could not be staged for deletion."))?;
    let previous_snapshot_version = catalog.snapshot_version;
    let removed_import = catalog
        .recording_imports
        .remove(&import_id)
        .expect("import existence was checked");
    let removed_artifact = catalog.import_artifacts.remove(&import_id);
    let removed_recording = removed_import
        .recording_id
        .as_ref()
        .and_then(|recording_id| catalog.recordings.remove(recording_id));
    let removed_recording_topics = removed_import
        .recording_id
        .as_ref()
        .and_then(|recording_id| catalog.topics_by_recording.remove(recording_id));
    let removed_recording_artifact = removed_import
        .recording_id
        .as_ref()
        .and_then(|recording_id| catalog.recording_artifacts.remove(recording_id));
    let receipt_keys = catalog
        .import_receipts
        .iter()
        .filter(|(_, receipt)| receipt.import_id == import_id)
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();
    let removed_receipts = receipt_keys
        .into_iter()
        .filter_map(|key| {
            catalog
                .import_receipts
                .remove(&key)
                .map(|receipt| (key, receipt))
        })
        .collect::<Vec<_>>();
    catalog.bump_version();
    let registry = catalog.durable_registry();
    if state
        .import_storage
        .persist_registry(registry)
        .await
        .is_err()
    {
        catalog
            .recording_imports
            .insert(import_id.clone(), removed_import);
        if let Some(artifact) = removed_artifact {
            catalog.import_artifacts.insert(import_id.clone(), artifact);
        }
        if let Some(recording) = removed_recording {
            catalog.recordings.insert(recording.id.clone(), recording);
        }
        if let (Some(recording_id), Some(topics)) =
            (import.recording_id.as_ref(), removed_recording_topics)
        {
            catalog
                .topics_by_recording
                .insert(recording_id.clone(), topics);
        }
        if let (Some(recording_id), Some(relative_path)) =
            (import.recording_id.as_ref(), removed_recording_artifact)
        {
            catalog
                .recording_artifacts
                .insert(recording_id.clone(), relative_path);
        }
        catalog.import_receipts.extend(removed_receipts);
        catalog.snapshot_version = previous_snapshot_version;
        catalog
            .workspace_version
            .send_replace(previous_snapshot_version);
        drop(catalog);
        state
            .import_storage
            .rollback_import_deletion(staged)
            .await
            .map_err(|_io| ApiError::internal("Import deletion rollback failed."))?;
        return Err(ApiError::internal(
            "Import deletion could not be persisted.",
        ));
    }
    drop(catalog);
    if let Err(err) = state.import_storage.commit_import_deletion(staged).await {
        // The durable catalog no longer references this same-volume trash entry. Startup
        // scavenging will retry deletion, so returning success is safer than an unretryable 500.
        eprintln!("Failed to remove staged import trash: {err}");
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn open_source_artifact(
    State(state): State<AppState>,
    AxumPath(import_id): AxumPath<String>,
    headers: HeaderMap,
) -> ApiResult<axum::response::Response> {
    let (artifact, source_sha256) = {
        let catalog = state.catalog.read().await;
        let import = catalog
            .recording_imports
            .get(&import_id)
            .ok_or_else(|| ApiError::not_found("RecordingImport", &import_id))?;
        if import.status == "cancelled" {
            return Err(ApiError::gone(
                "The cancelled upload artifact is unavailable.",
            ));
        }
        let artifact = catalog
            .import_artifacts
            .get(&import_id)
            .cloned()
            .ok_or_else(|| ApiError::not_found("ImportArtifact", &import_id))?;
        let source_sha256 = import
            .source_sha256
            .clone()
            .ok_or_else(|| ApiError::conflict("Import upload is not complete."))?;
        (artifact, source_sha256)
    };
    state
        .import_storage
        .file_response(
            &artifact.source_relative_path,
            &headers,
            &artifact.source_content_type,
            &source_sha256,
            false,
        )
        .await
        .map_err(|_io| ApiError::internal("Import artifact could not be read."))
}

struct PreparedAssignment {
    organization_id: String,
    data_source_id: String,
    project_snapshot: RecordingProjectSnapshot,
    topic_ids: Vec<String>,
    mapping_version: u64,
}

async fn validate_import_scope(
    state: &AppState,
    project_id: &str,
    device_id: &str,
    requested_source_id: Option<&str>,
) -> ApiResult<()> {
    let catalog = state.catalog.read().await;
    let project = catalog
        .projects
        .get(project_id)
        .ok_or_else(|| ApiError::not_found("Project", project_id))?;
    let device = catalog
        .devices
        .get(device_id)
        .ok_or_else(|| ApiError::not_found("Device", device_id))?;
    if project.organization_id != device.organization_id {
        return Err(ApiError::conflict(
            "Project and Device must belong to the same organization.",
        ));
    }
    let device_is_assigned = catalog.device_assignments.values().any(|assignment| {
        assignment.project_id == project_id
            && assignment.device_id == device_id
            && assignment.valid_to.is_none()
    });
    if !device_is_assigned {
        return Err(ApiError::conflict(
            "Device must have an active assignment to the Project.",
        ));
    }
    if let Some(source_id) = requested_source_id {
        let source = catalog
            .data_sources
            .get(source_id)
            .ok_or_else(|| ApiError::not_found("DataSource", source_id))?;
        if source.device_id != device_id {
            return Err(ApiError::conflict(
                "DataSource does not belong to the selected Device.",
            ));
        }
        if source.protocol != "file" {
            return Err(ApiError::conflict(
                "Imports require a file DataSource and cannot modify a Live DataSource.",
            ));
        }
        let source_is_assigned = catalog.data_assignments.values().any(|assignment| {
            assignment.project_id == project_id
                && assignment.data_source_id == source_id
                && assignment.valid_to.is_none()
        });
        if !source_is_assigned {
            return Err(ApiError::conflict(
                "DataSource must have an active assignment to the Project.",
            ));
        }
    }
    Ok(())
}

async fn prepare_import_assignment(
    state: &AppState,
    project_id: &str,
    device_id: &str,
    requested_source_id: Option<&str>,
) -> ApiResult<PreparedAssignment> {
    let mut catalog = state.catalog.write().await;
    let project = catalog
        .projects
        .get(project_id)
        .cloned()
        .ok_or_else(|| ApiError::not_found("Project", project_id))?;
    let device = catalog
        .devices
        .get(device_id)
        .cloned()
        .ok_or_else(|| ApiError::not_found("Device", device_id))?;
    if project.organization_id != device.organization_id {
        return Err(ApiError::conflict(
            "Project and Device must belong to the same organization.",
        ));
    }
    let device_assignment = catalog
        .device_assignments
        .values()
        .find(|assignment| {
            assignment.project_id == project_id
                && assignment.device_id == device_id
                && assignment.valid_to.is_none()
        })
        .cloned()
        .ok_or_else(|| {
            ApiError::conflict("Device must have an active assignment to the Project.")
        })?;

    let (source, data_assignment) = if let Some(source_id) = requested_source_id {
        let source = catalog
            .data_sources
            .get(source_id)
            .cloned()
            .ok_or_else(|| ApiError::not_found("DataSource", source_id))?;
        if source.device_id != device_id {
            return Err(ApiError::conflict(
                "DataSource does not belong to the selected Device.",
            ));
        }
        if source.protocol != "file" {
            return Err(ApiError::conflict(
                "Imports require a file DataSource and cannot modify a Live DataSource.",
            ));
        }
        let assignment = catalog
            .data_assignments
            .values()
            .find(|assignment| {
                assignment.project_id == project_id
                    && assignment.data_source_id == source_id
                    && assignment.valid_to.is_none()
            })
            .cloned()
            .ok_or_else(|| {
                ApiError::conflict("DataSource must have an active assignment to the Project.")
            })?;
        (source, assignment)
    } else {
        let source = catalog
            .data_sources
            .values()
            .find(|source| source.device_id == device_id && source.protocol == "file")
            .cloned()
            .unwrap_or_else(|| DataSource {
                id: new_id("import-source"),
                integration_id: device.integration_id.clone(),
                device_id: device.id.clone(),
                name: format!("{} 파일 Import", device.name),
                protocol: "file".to_owned(),
                status: "ready".to_owned(),
                live_url: String::new(),
                topic_ids: Vec::new(),
                mapping_version: 1,
                last_data_at: now_iso(),
            });
        let source_was_created = !catalog.data_sources.contains_key(&source.id);
        if source_was_created {
            catalog
                .data_sources
                .insert(source.id.clone(), source.clone());
        }
        let assignment = catalog
            .data_assignments
            .values()
            .find(|assignment| {
                assignment.project_id == project_id
                    && assignment.data_source_id == source.id
                    && assignment.valid_to.is_none()
            })
            .cloned()
            .unwrap_or_else(|| DataAssignment {
                id: new_id("data-assignment"),
                project_id: project.id.clone(),
                data_source_id: source.id.clone(),
                visibility: DataVisibility::Operator,
                valid_from: now_iso(),
                valid_to: None,
                resource_version: catalog.snapshot_version.saturating_add(1),
            });
        let assignment_was_created = !catalog.data_assignments.contains_key(&assignment.id);
        if assignment_was_created {
            catalog
                .data_assignments
                .insert(assignment.id.clone(), assignment.clone());
        }
        if source_was_created || assignment_was_created {
            catalog.bump_version();
        }
        (source, assignment)
    };

    Ok(PreparedAssignment {
        organization_id: project.organization_id,
        data_source_id: source.id,
        project_snapshot: RecordingProjectSnapshot {
            project_id: project.id,
            project_name: project.name,
            captured_at: now_iso(),
            device_assignment_id: device_assignment.id,
            data_assignment_id: data_assignment.id,
        },
        topic_ids: source.topic_ids,
        mapping_version: source.mapping_version,
    })
}

fn spawn_import_worker(
    state: AppState,
    import: RecordingImport,
    file: UploadedFile,
    mapping: Option<serde_json::Value>,
    expected_resource_version: u64,
    cancellation: ImportCancellation,
    import_permit: OwnedSemaphorePermit,
) {
    tokio::spawn(async move {
        let _import_permit = import_permit;
        let output_relative_path =
            crate::import_storage::ImportStorage::relative_rrd_path(&import.id);
        let output_path = match state
            .import_storage
            .resolve_for_write(&output_relative_path)
        {
            Ok(path) => path,
            Err(err) => {
                finish_import_failure(
                    &state,
                    &import.id,
                    expected_resource_version,
                    "Import storage could not create the RRD artifact.",
                    &err.to_string(),
                )
                .await;
                finalize_worker(&state, &import.id).await;
                return;
            }
        };
        let source_path = match state.import_storage.resolve_for_write(&file.relative_path) {
            Ok(path) => path,
            Err(err) => {
                finish_import_failure(
                    &state,
                    &import.id,
                    expected_resource_version,
                    "The uploaded artifact is unavailable.",
                    &err.to_string(),
                )
                .await;
                finalize_worker(&state, &import.id).await;
                return;
            }
        };
        let max_output_bytes = state.import_storage.max_upload_bytes();
        let Ok(mut output_reservation) =
            OutputReservation::reserve(Arc::clone(&state.import_storage), max_output_bytes)
        else {
            finish_import_failure(
                &state,
                &import.id,
                expected_resource_version,
                "RMS storage does not have enough space for the converted recording.",
                "The configured RMS storage quota rejected the output reservation.",
            )
            .await;
            finalize_worker(&state, &import.id).await;
            return;
        };
        let request = ImportRequest {
            source_path,
            output_rrd_path: output_path.clone(),
            original_file_name: file.file_name.clone(),
            format: file.format,
            mapping,
            max_output_bytes,
            cancellation: Some(cancellation),
        };
        let result = tokio::task::spawn_blocking(move || rms_import::run_import(&request)).await;
        match result {
            Ok(Ok(result)) => {
                let result_size = result.size_bytes;
                if result_size > max_output_bytes {
                    drop(tokio::fs::remove_file(output_path).await);
                    finish_import_failure(
                        &state,
                        &import.id,
                        expected_resource_version,
                        "The converted recording exceeds the output limit.",
                        "Importer returned an artifact larger than max_output_bytes.",
                    )
                    .await;
                    drop(output_reservation);
                    finalize_worker(&state, &import.id).await;
                    return;
                }
                let committed = finish_import_success(
                    &state,
                    &import.id,
                    expected_resource_version,
                    &file.sha256,
                    &output_relative_path,
                    result,
                )
                .await;
                if committed {
                    output_reservation.keep_actual(result_size);
                }
            }
            Ok(Err(err)) => {
                drop(tokio::fs::remove_file(output_path).await);
                finish_import_failure(
                    &state,
                    &import.id,
                    expected_resource_version,
                    err.user_reason(),
                    err.internal_detail(),
                )
                .await;
            }
            Err(err) => {
                drop(tokio::fs::remove_file(output_path).await);
                finish_import_failure(
                    &state,
                    &import.id,
                    expected_resource_version,
                    "The import worker stopped unexpectedly.",
                    &err.to_string(),
                )
                .await;
            }
        }
        drop(output_reservation);
        finalize_worker(&state, &import.id).await;
    });
}

async fn finalize_worker(state: &AppState, import_id: &str) {
    let cleanup_cancelled = state
        .catalog
        .read()
        .await
        .recording_imports
        .get(import_id)
        .is_some_and(|import| import.status == "cancelled");
    if cleanup_cancelled && let Err(err) = state.import_storage.remove_import_dir(import_id).await {
        eprintln!("Failed to remove cancelled import artifacts: {err}");
        return;
    }
    {
        let mut catalog = state.catalog.write().await;
        catalog.import_cancellations.remove(import_id);
        let is_still_cancelled = catalog
            .recording_imports
            .get(import_id)
            .is_some_and(|import| import.status == "cancelled");
        if cleanup_cancelled && is_still_cancelled {
            let resource_version = catalog.bump_version();
            let import = catalog
                .recording_imports
                .get_mut(import_id)
                .expect("cancelled import existence was checked");
            import.progress_percent = 100;
            import.artifact_url = None;
            import.updated_at = now_iso();
            import.resource_version = resource_version;
            catalog.import_artifacts.remove(import_id);
        }
    }
    if cleanup_cancelled && let Err(err) = state.persist_registry().await {
        eprintln!("Failed to persist cancelled import cleanup: {err}");
    }
}

async fn finish_import_success(
    state: &AppState,
    import_id: &str,
    expected_resource_version: u64,
    expected_source_sha256: &str,
    output_relative_path: &str,
    result: ImportResult,
) -> bool {
    if result.source_sha256 != expected_source_sha256 || !result.footer_verified {
        drop(tokio::fs::remove_file(&result.rrd_path).await);
        finish_import_failure(
            state,
            import_id,
            expected_resource_version,
            "The converted recording did not pass integrity verification.",
            "Importer result source hash or footer verification did not match.",
        )
        .await;
        return false;
    }

    let result_rrd_path = result.rrd_path.clone();
    let persist_guard = state.registry_persist_lock.lock().await;
    let mut catalog = state.catalog.write().await;
    let Some(current) = catalog.recording_imports.get(import_id).cloned() else {
        drop(catalog);
        drop(persist_guard);
        drop(tokio::fs::remove_file(&result_rrd_path).await);
        return false;
    };
    if current.status != "processing" || current.resource_version != expected_resource_version {
        drop(catalog);
        drop(persist_guard);
        drop(tokio::fs::remove_file(&result_rrd_path).await);
        return false;
    }
    let Some(stored) = catalog.import_artifacts.get(import_id).cloned() else {
        drop(catalog);
        drop(persist_guard);
        drop(tokio::fs::remove_file(&result_rrd_path).await);
        return false;
    };
    let device_assignment_is_active = catalog
        .device_assignments
        .get(&stored.project_snapshot.device_assignment_id)
        .is_some_and(|assignment| {
            assignment.project_id == current.project_id
                && assignment.device_id == current.device_id
                && assignment.valid_to.is_none()
        });
    let data_assignment_is_active = catalog
        .data_assignments
        .get(&stored.project_snapshot.data_assignment_id)
        .is_some_and(|assignment| {
            assignment.project_id == current.project_id
                && assignment.data_source_id == current.data_source_id
                && assignment.valid_to.is_none()
        });
    if !device_assignment_is_active || !data_assignment_is_active {
        drop(catalog);
        drop(persist_guard);
        drop(tokio::fs::remove_file(&result_rrd_path).await);
        finish_import_failure(
            state,
            import_id,
            expected_resource_version,
            "Project assignments changed before the import completed.",
            "The captured DeviceAssignment or DataAssignment is no longer active.",
        )
        .await;
        return false;
    }
    let previous_snapshot_version = catalog.snapshot_version;
    let previous_source = catalog.data_sources.get(&current.data_source_id).cloned();
    let previous_topics = catalog
        .topics_by_data_source
        .get(&current.data_source_id)
        .cloned();
    let resource_version = catalog.bump_version();
    let recording_id = new_id("recording");
    let imported_topics = if result.entity_paths.is_empty() {
        &result.topics
    } else {
        &result.entity_paths
    };
    let raw_topics = imported_topics
        .iter()
        .filter(|topic| !topic.trim().is_empty())
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let source_identity = catalog
        .data_sources
        .get(&current.data_source_id)
        .map(|source| (source.id.clone(), source.device_id.clone()));
    let materialized_topics = source_identity.as_ref().and_then(|(source_id, device_id)| {
        (!raw_topics.is_empty()).then(|| {
            if result.entity_descriptors.is_empty() {
                topics_for_source(source_id, device_id, &raw_topics)
            } else {
                topics_for_entity_descriptors(source_id, device_id, &result.entity_descriptors)
            }
        })
    });
    let topics = materialized_topics.as_ref().map_or_else(
        || {
            catalog
                .data_sources
                .get(&current.data_source_id)
                .map_or_else(
                    || stored.topic_ids.clone(),
                    |source| source.topic_ids.clone(),
                )
        },
        |topics| topics.iter().map(|topic| topic.id.clone()).collect(),
    );
    let recording_topics = materialized_topics.clone().unwrap_or_else(|| {
        catalog
            .topics_by_data_source
            .get(&current.data_source_id)
            .map(|source_topics| {
                source_topics
                    .iter()
                    .filter(|topic| topics.contains(&topic.id))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    });
    let source_topic_update = catalog
        .data_sources
        .get_mut(&current.data_source_id)
        .map(|source| {
            let schema_changed = source.topic_ids != topics;
            source.topic_ids.clone_from(&topics);
            source.last_data_at = now_iso();
            if schema_changed {
                source.mapping_version = source.mapping_version.saturating_add(1);
            }
            (source.id.clone(), source.mapping_version)
        });
    let mapping_version = source_topic_update
        .as_ref()
        .map_or(stored.mapping_version, |(_, version)| *version);
    if let (Some((source_id, _)), Some(materialized_topics)) =
        (source_topic_update, materialized_topics)
    {
        catalog
            .topics_by_data_source
            .insert(source_id, materialized_topics);
    }
    let timelines = result
        .timelines
        .into_iter()
        .map(|timeline| TimelineDescriptor {
            name: timeline.name,
            kind: match timeline.kind {
                TimelineKind::Sequence => "sequence",
                TimelineKind::Timestamp => "timestamp",
                TimelineKind::Duration => "duration",
            }
            .to_owned(),
            start: timeline.start,
            end: timeline.end,
            duration_seconds: timeline.duration_seconds,
            fps: timeline.fps,
        })
        .collect::<Vec<_>>();
    let duration_seconds = result.duration_seconds;
    let recording = Recording {
        id: recording_id.clone(),
        organization_id: stored.organization_id.clone(),
        project_id: current.project_id.clone(),
        device_id: current.device_id.clone(),
        data_source_id: current.data_source_id.clone(),
        name: recording_name(&current.file_name),
        status: "ready".to_owned(),
        rrd_url: format!("/rerun/recordings/{recording_id}"),
        captured_at: now_iso(),
        duration_label: duration_label(duration_seconds),
        timelines,
        default_timeline: result.default_timeline,
        duration_seconds,
        rrd_version: result.rrd_version,
        footer_verified: result.footer_verified,
        content_sha256: result.content_sha256.clone(),
        topic_ids: topics.clone(),
        mapping_version,
        project_snapshot: stored.project_snapshot.clone(),
        resource_version,
        manifest_hash: format!("sha256:{}", result.content_sha256),
    };

    catalog
        .recording_artifacts
        .insert(recording_id.clone(), output_relative_path.to_owned());
    catalog
        .topics_by_recording
        .insert(recording_id.clone(), recording_topics);
    catalog.recordings.insert(recording_id.clone(), recording);
    if let Some(artifact) = catalog.import_artifacts.get_mut(import_id) {
        artifact.rrd_relative_path = Some(output_relative_path.to_owned());
        artifact.internal_detail = None;
        artifact.topic_ids = topics;
        artifact.mapping_version = mapping_version;
    }
    let import = catalog
        .recording_imports
        .get_mut(import_id)
        .expect("import existence was checked");
    import.status = "ready".to_owned();
    import.progress_percent = 100;
    import.recording_id = Some(recording_id.clone());
    import.failure_reason = None;
    import.warnings = result.warnings;
    import.updated_at = now_iso();
    import.resource_version = resource_version;

    let registry = catalog.durable_registry();
    if state
        .import_storage
        .persist_registry(registry)
        .await
        .is_err()
    {
        catalog.recordings.remove(&recording_id);
        catalog.topics_by_recording.remove(&recording_id);
        catalog.recording_artifacts.remove(&recording_id);
        if let Some(source) = previous_source {
            catalog
                .data_sources
                .insert(current.data_source_id.clone(), source);
        }
        if let Some(topics) = previous_topics {
            catalog
                .topics_by_data_source
                .insert(current.data_source_id.clone(), topics);
        } else {
            catalog
                .topics_by_data_source
                .remove(&current.data_source_id);
        }
        catalog
            .import_artifacts
            .insert(import_id.to_owned(), stored);
        let failed_version = previous_snapshot_version.saturating_add(1);
        catalog.snapshot_version = failed_version;
        catalog.workspace_version.send_replace(failed_version);
        let import = catalog
            .recording_imports
            .get_mut(import_id)
            .expect("import existence was checked");
        *import = current;
        import.status = "failed".to_owned();
        import.progress_percent = 100;
        import.failure_reason =
            Some("Import metadata could not be persisted. Try the import again.".to_owned());
        import.updated_at = now_iso();
        import.resource_version = failed_version;
        drop(catalog);
        drop(persist_guard);
        drop(tokio::fs::remove_file(&result_rrd_path).await);
        return false;
    }
    drop(catalog);
    drop(persist_guard);
    true
}

async fn finish_import_failure(
    state: &AppState,
    import_id: &str,
    expected_resource_version: u64,
    user_reason: &str,
    internal_detail: &str,
) {
    let changed = {
        let mut catalog = state.catalog.write().await;
        let is_current = catalog
            .recording_imports
            .get(import_id)
            .is_some_and(|current| {
                current.status == "processing"
                    && current.resource_version == expected_resource_version
            });
        if !is_current {
            false
        } else {
            let resource_version = catalog.bump_version();
            let current = catalog
                .recording_imports
                .get_mut(import_id)
                .expect("import existence was checked");
            current.status = "failed".to_owned();
            current.progress_percent = 100;
            current.failure_reason = Some(user_reason.to_owned());
            current.updated_at = now_iso();
            current.resource_version = resource_version;
            if let Some(artifact) = catalog.import_artifacts.get_mut(import_id) {
                artifact.internal_detail = Some(internal_detail.to_owned());
            }
            true
        }
    };
    if changed && let Err(err) = state.persist_registry().await {
        eprintln!("Failed to persist failed import: {err}");
    }
}

async fn write_upload(
    state: &AppState,
    import_id: &str,
    mut field: Field<'_>,
    requested_format: &str,
    cleanup: &mut ImportDirGuard,
    upload_deadline: UploadDeadline,
) -> ApiResult<UploadedFile> {
    let file_name = normalize_file_name(
        field
            .file_name()
            .ok_or_else(|| ApiError::bad_request("file must include a filename."))?,
    )?;
    let (format, format_label, content_type) = format_from_file_name(&file_name)?;
    if requested_format != format_label {
        return Err(ApiError::unsupported_media_type(format!(
            "The selected format `{requested_format}` does not match the uploaded file."
        )));
    }
    let relative_path = crate::import_storage::ImportStorage::relative_source_path(import_id);
    let path = state
        .import_storage
        .resolve_for_write(&relative_path)
        .map_err(|_io| ApiError::bad_request("The uploaded filename is not safe."))?;
    let mut destination = tokio::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .await
        .map_err(|_io| ApiError::internal("The uploaded file could not be stored."))?;
    let mut hasher = Sha256::new();
    let mut size_bytes = 0_u64;
    let mut prefix = Vec::with_capacity(16);
    while let Some(chunk) = upload_deadline.next_chunk(&mut field).await? {
        size_bytes = size_bytes
            .checked_add(chunk.len() as u64)
            .ok_or_else(|| ApiError::payload_too_large("The uploaded file is too large."))?;
        if size_bytes > state.import_storage.max_upload_bytes() {
            return Err(ApiError::payload_too_large(format!(
                "The uploaded file exceeds the {} byte limit.",
                state.import_storage.max_upload_bytes()
            )));
        }
        cleanup.reserve(chunk.len() as u64)?;
        let prefix_remaining = 16_usize.saturating_sub(prefix.len());
        prefix.extend_from_slice(&chunk[..chunk.len().min(prefix_remaining)]);
        hasher.update(&chunk);
        destination
            .write_all(&chunk)
            .await
            .map_err(|_io| ApiError::internal("The uploaded file could not be stored."))?;
    }
    tokio::time::timeout_at(upload_deadline.overall, destination.sync_all())
        .await
        .map_err(|_elapsed| ApiError::request_timeout("The multipart upload timed out."))?
        .map_err(|_io| ApiError::internal("The uploaded file could not be finalized."))?;
    if size_bytes == 0 {
        return Err(ApiError::bad_request("The uploaded file is empty."));
    }
    validate_magic(format, &file_name, &prefix)?;
    Ok(UploadedFile {
        file_name,
        relative_path,
        format,
        format_label: format_label.to_owned(),
        content_type: content_type.to_owned(),
        size_bytes,
        sha256: format!("{:x}", hasher.finalize()),
    })
}

async fn read_text(
    mut field: Field<'_>,
    max_bytes: usize,
    upload_deadline: UploadDeadline,
) -> ApiResult<String> {
    let mut bytes = Vec::new();
    while let Some(chunk) = upload_deadline.next_chunk(&mut field).await? {
        if bytes.len().saturating_add(chunk.len()) > max_bytes {
            return Err(ApiError::payload_too_large(
                "A multipart text field is too large.",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes)
        .map(|value| value.trim().to_owned())
        .map_err(|_utf8| ApiError::bad_request("Multipart text fields must be UTF-8."))
}

fn set_once(slot: &mut Option<String>, value: String, name: &str) -> ApiResult<()> {
    if slot.replace(value).is_some() {
        return Err(ApiError::bad_request(format!(
            "{name} must not be repeated."
        )));
    }
    Ok(())
}

fn required(value: Option<String>, name: &str) -> ApiResult<String> {
    value
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::bad_request(format!("{name} is required.")))
}

fn normalize_file_name(file_name: &str) -> ApiResult<String> {
    let file_name = file_name.trim();
    let path = Path::new(file_name);
    let is_single_normal_component = !file_name.is_empty()
        && file_name.len() <= 255
        && !file_name.chars().any(char::is_control)
        && !file_name.contains(['/', '\\'])
        && path.file_name().is_some_and(|name| name == file_name)
        && file_name != "."
        && file_name != "..";
    if !is_single_normal_component {
        return Err(ApiError::bad_request(
            "The uploaded filename must be a single safe path component.",
        ));
    }
    Ok(file_name.to_owned())
}

fn format_from_file_name(file_name: &str) -> ApiResult<(ImportFormat, &'static str, &'static str)> {
    let extension = Path::new(file_name)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| ApiError::unsupported_media_type("The file extension is required."))?;
    match extension.as_str() {
        "rrd" => Ok((ImportFormat::Rrd, "rrd", "application/x-rerun")),
        "mcap" => Ok((ImportFormat::Mcap, "mcap", "application/octet-stream")),
        "zip" => Ok((ImportFormat::Ros2BagZip, "ros2-bag-zip", "application/zip")),
        "csv" => Ok((ImportFormat::Csv, "csv", "text/csv; charset=utf-8")),
        "mp4" => Ok((ImportFormat::Video, "video", "video/mp4")),
        "mov" => Ok((ImportFormat::Video, "video", "video/quicktime")),
        "webm" => Ok((ImportFormat::Video, "video", "video/webm")),
        _ => Err(ApiError::unsupported_media_type(
            "Supported files are RRD, MCAP, ROS 2 bag ZIP, CSV, MP4, MOV, and WebM.",
        )),
    }
}

fn validate_magic(format: ImportFormat, file_name: &str, prefix: &[u8]) -> ApiResult<()> {
    let extension = Path::new(file_name)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let valid = match format {
        ImportFormat::Rrd => matches!(prefix.get(..4), Some(b"RRF0" | b"RRF1" | b"RRF2")),
        ImportFormat::Mcap => prefix.starts_with(b"\x89MCAP0\r\n"),
        ImportFormat::Ros2BagZip => {
            prefix.starts_with(b"PK\x03\x04")
                || prefix.starts_with(b"PK\x05\x06")
                || prefix.starts_with(b"PK\x07\x08")
        }
        ImportFormat::Csv => !prefix.contains(&0),
        ImportFormat::Video if extension == "webm" => prefix.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]),
        ImportFormat::Video => prefix.get(4..8) == Some(b"ftyp"),
    };
    if !valid {
        return Err(ApiError::unsupported_media_type(
            "The file signature does not match its selected format.",
        ));
    }
    Ok(())
}

fn duration_label(duration_seconds: f64) -> String {
    let duration_seconds = duration_seconds.max(0.0);
    let minutes = (duration_seconds / 60.0).floor() as u64;
    let seconds = duration_seconds - (minutes as f64 * 60.0);
    if (seconds.fract()).abs() < f64::EPSILON {
        format!("{minutes:02}:{seconds:02.0}")
    } else {
        format!("{minutes:02}:{seconds:04.1}")
    }
}

fn recording_name(file_name: &str) -> String {
    Path::new(file_name)
        .file_stem()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("Imported recording")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse as _;

    #[test]
    fn rejects_traversal_filenames() {
        assert!(normalize_file_name("../escape.rrd").is_err());
        assert!(normalize_file_name("nested/file.rrd").is_err());
        assert!(normalize_file_name("nested\\file.rrd").is_err());
        assert_eq!(normalize_file_name("CON.rrd").unwrap(), "CON.rrd");
        assert_eq!(
            normalize_file_name("capture.rrd. ").unwrap(),
            "capture.rrd."
        );
        assert_eq!(
            normalize_file_name("로봇 기록.rrd").unwrap(),
            "로봇 기록.rrd"
        );
        assert_eq!(
            crate::import_storage::ImportStorage::relative_source_path("safe-id"),
            "imports/safe-id/source/upload.bin"
        );
    }

    #[test]
    fn recognizes_supported_formats_without_relabeling_video() {
        assert_eq!(format_from_file_name("capture.rrd").unwrap().1, "rrd");
        assert_eq!(
            format_from_file_name("capture.zip").unwrap().1,
            "ros2-bag-zip"
        );
        assert_eq!(format_from_file_name("capture.mov").unwrap().1, "video");
        assert_eq!(format_from_file_name("capture.webm").unwrap().1, "video");
    }

    #[tokio::test]
    async fn import_admission_wait_is_bounded_by_the_upload_deadline() {
        let state = AppState::fixture();
        let permit_count = state.import_slots.available_permits();
        assert!(permit_count > 0);
        let mut held = Vec::with_capacity(permit_count);
        for _ in 0..permit_count {
            held.push(state.import_slots.clone().acquire_owned().await.unwrap());
        }
        let deadline = UploadDeadline {
            overall: Instant::now() + Duration::from_millis(10),
            idle_timeout: Duration::from_millis(10),
        };
        assert!(acquire_import_permit(&state, deadline).await.is_err());
        drop(held);
    }

    #[tokio::test]
    async fn cancellation_persist_failure_keeps_processing_import_and_artifacts() {
        let storage_root = tempfile::tempdir().expect("temporary storage is available");
        let state =
            AppState::fixture_with_storage(storage_root.path()).expect("fixture storage opens");
        let import_id = "recording-import-persist-failure";
        state
            .import_storage
            .create_import_dirs(import_id)
            .await
            .expect("import directory is created");
        let source_relative_path =
            crate::import_storage::ImportStorage::relative_source_path(import_id);
        let source_path = state
            .import_storage
            .resolve_for_write(&source_relative_path)
            .expect("source path is safe");
        tokio::fs::write(&source_path, b"source")
            .await
            .expect("source artifact is written");

        let cancellation = ImportCancellation::new();
        let (snapshot_version, resource_version) = {
            let mut catalog = state.catalog.write().await;
            let resource_version = catalog.bump_version();
            catalog.recording_imports.insert(
                import_id.to_owned(),
                RecordingImport {
                    id: import_id.to_owned(),
                    project_id: "project-logistics".to_owned(),
                    device_id: "robot-07".to_owned(),
                    data_source_id: "import-source".to_owned(),
                    file_name: "capture.rrd".to_owned(),
                    format: "rrd".to_owned(),
                    status: "processing".to_owned(),
                    progress_percent: 10,
                    size_bytes: 6,
                    source_sha256: Some("source-hash".to_owned()),
                    artifact_url: Some(format!("/api/v1/recording-imports/{import_id}/artifact")),
                    recording_id: None,
                    failure_reason: None,
                    warnings: Vec::new(),
                    created_at: "2026-08-26T00:00:00Z".to_owned(),
                    updated_at: "2026-08-26T00:00:00Z".to_owned(),
                    resource_version,
                },
            );
            catalog.import_artifacts.insert(
                import_id.to_owned(),
                StoredImportArtifact {
                    source_relative_path,
                    rrd_relative_path: None,
                    source_content_type: "application/octet-stream".to_owned(),
                    internal_detail: None,
                    organization_id: "organization-acme".to_owned(),
                    project_snapshot: RecordingProjectSnapshot {
                        project_id: "project-logistics".to_owned(),
                        project_name: "Logistics".to_owned(),
                        captured_at: "2026-08-26T00:00:00Z".to_owned(),
                        device_assignment_id: "device-assignment".to_owned(),
                        data_assignment_id: "data-assignment".to_owned(),
                    },
                    topic_ids: Vec::new(),
                    mapping_version: 1,
                },
            );
            catalog
                .import_cancellations
                .insert(import_id.to_owned(), cancellation.clone());
            (catalog.snapshot_version, resource_version)
        };
        state
            .persist_registry()
            .await
            .expect("initial registry persists");

        let registry_path = storage_root.path().join("registry.json");
        std::fs::remove_file(&registry_path).expect("registry file is removed");
        std::fs::create_dir(&registry_path).expect("registry destination is blocked");

        let error = delete_import(State(state.clone()), AxumPath(import_id.to_owned()))
            .await
            .expect_err("cancellation persistence must fail");
        assert_eq!(
            error.into_response().status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert!(!cancellation.is_cancelled());

        let catalog = state.catalog.read().await;
        let import = catalog
            .recording_imports
            .get(import_id)
            .expect("processing import is restored");
        assert_eq!(import.status, "processing");
        assert_eq!(import.progress_percent, 10);
        assert_eq!(import.resource_version, resource_version);
        assert_eq!(catalog.snapshot_version, snapshot_version);
        assert_eq!(*catalog.workspace_version.borrow(), snapshot_version);
        assert!(catalog.import_artifacts.contains_key(import_id));
        assert!(catalog.import_cancellations.contains_key(import_id));
        assert!(source_path.exists());
    }
}
