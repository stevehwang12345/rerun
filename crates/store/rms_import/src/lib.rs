//! Converts explicitly supported RMS upload formats into verified, footer-backed RRD recordings.
//!
//! This crate is a blocking service boundary intended to be called from `spawn_blocking`.
//! It never invokes a shell and never publishes a partially-written destination file.

mod csv_import;
mod external_tools;
mod metadata;
mod rosbag;
mod video;

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

/// Source format selected by the upload service after MIME/extension inspection.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportFormat {
    Rrd,
    Mcap,
    Ros2BagZip,
    Csv,
    Video,
}

/// A blocking import request.
#[derive(Clone, Debug)]
pub struct ImportRequest {
    pub source_path: PathBuf,
    pub output_rrd_path: PathBuf,
    pub original_file_name: String,
    pub format: ImportFormat,
    /// Format-specific JSON. Only CSV currently accepts a mapping; other formats reject it.
    pub mapping: Option<serde_json::Value>,
    /// Hard cap applied while writing the RRD, including its footer.
    pub max_output_bytes: u64,
    /// Cooperative cancellation checked before commit and throughout bounded conversion steps.
    pub cancellation: Option<ImportCancellation>,
}

/// Cloneable cancellation signal owned by the upload service.
#[derive(Clone, Debug, Default)]
pub struct ImportCancellation(Arc<AtomicBool>);

impl ImportCancellation {
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    #[inline]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// Rerun timeline kind preserved without lossy relabeling.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineKind {
    Sequence,
    Timestamp,
    Duration,
}

/// Lossless timeline bounds extracted from the verified RRD footer.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineDescriptor {
    pub name: String,
    pub kind: TimelineKind,
    pub start: String,
    pub end: String,
    pub duration_seconds: Option<f64>,
    pub fps: Option<f64>,
}

/// Viewer category derived from the verified RRD component archetypes for one entity.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityVisualization {
    Camera,
    /// Geographic points or lines rendered on the map view.
    Map,
    Spatial2d,
    Spatial3d,
    /// A transform hierarchy that should be shown with explicit coordinate axes.
    Transform3d,
    TimeSeries,
    State,
    Log,
    /// Verified data for which no registered semantic visualizer was found.
    ///
    /// The product viewer presents these components in a generic dataframe instead of routing
    /// them to a misleading empty text-log or spatial view.
    Raw,
}

impl EntityVisualization {
    /// Stable RMS renderer name used by Recording Topic descriptors.
    #[inline]
    pub fn renderer(self) -> &'static str {
        match self {
            Self::Camera => "camera",
            Self::Map => "map",
            Self::Spatial2d => "spatial2d",
            Self::Spatial3d => "spatial3d",
            Self::Transform3d => "transform3d",
            Self::TimeSeries => "timeseries",
            Self::State => "state",
            Self::Log => "log",
            Self::Raw => "raw",
        }
    }
}

/// Immutable visualization metadata for an entity in a verified Recording.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityDescriptor {
    pub path: String,
    pub visualization: EntityVisualization,
}

/// Metadata returned only after the destination RRD has passed footer verification.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportResult {
    pub rrd_path: PathBuf,
    pub source_sha256: String,
    pub content_sha256: String,
    pub size_bytes: u64,
    pub rrd_version: String,
    pub footer_verified: bool,
    pub timelines: Vec<TimelineDescriptor>,
    pub default_timeline: String,
    pub duration_seconds: f64,
    pub warnings: Vec<String>,
    pub entity_paths: Vec<String>,
    pub entity_descriptors: Vec<EntityDescriptor>,
    pub topics: Vec<String>,
}

/// Stable machine-readable import failure category.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportErrorCode {
    InvalidRequest,
    UnsupportedFormat,
    ValidationFailed,
    ResourceLimitExceeded,
    ToolUnavailable,
    ConversionFailed,
    OutputValidationFailed,
    Io,
    Cancelled,
}

/// Import failure with separately consumable user-facing and internal details.
#[derive(Debug, thiserror::Error)]
#[error("{user_reason}")]
pub struct ImportError {
    code: ImportErrorCode,
    user_reason: String,
    internal_detail: String,
}

impl ImportError {
    #[inline]
    pub fn code(&self) -> ImportErrorCode {
        self.code
    }

    #[inline]
    pub fn user_reason(&self) -> &str {
        &self.user_reason
    }

    #[inline]
    pub fn internal_detail(&self) -> &str {
        &self.internal_detail
    }

    fn new(
        code: ImportErrorCode,
        user_reason: impl Into<String>,
        internal_detail: impl Into<String>,
    ) -> Self {
        Self {
            code,
            user_reason: user_reason.into(),
            internal_detail: internal_detail.into(),
        }
    }

    fn cancelled() -> Self {
        Self::new(
            ImportErrorCode::Cancelled,
            "The import was cancelled.",
            "Import cancellation was requested before commit",
        )
    }
}

/// Converts one source into an atomic, verified RRD destination.
pub fn run_import(request: &ImportRequest) -> Result<ImportResult, ImportError> {
    let validated = validate_request(request)?;
    check_cancelled(request.cancellation.as_ref())?;
    let source_sha256 =
        metadata::sha256_file(&validated.source_path, request.cancellation.as_ref())?;
    check_cancelled(request.cancellation.as_ref())?;

    let output_parent = validated.output_path.parent().ok_or_else(|| {
        ImportError::new(
            ImportErrorCode::InvalidRequest,
            "The output RRD path has no parent directory.",
            format!(
                "Output path has no parent: {}",
                validated.output_path.display()
            ),
        )
    })?;
    let mut temporary = tempfile::NamedTempFile::new_in(output_parent).map_err(|err| {
        ImportError::new(
            ImportErrorCode::Io,
            "The import worker could not create the destination recording.",
            format!(
                "Failed to create temporary RRD: {err}\nDirectory: {}",
                output_parent.display()
            ),
        )
    })?;
    let mut hints = ConversionHints::default();

    if request.format == ImportFormat::Rrd {
        metadata::verify_rrd(
            &validated.source_path,
            None,
            &BTreeMap::new(),
            request.cancellation.as_ref(),
        )?;
        copy_rrd(
            &validated.source_path,
            temporary.as_file_mut(),
            request.max_output_bytes,
            request.cancellation.as_ref(),
        )?;
    } else {
        let mut writer = RecordingWriter::new(
            temporary.as_file_mut(),
            request.max_output_bytes,
            &validated.recording_name,
            request.cancellation.clone(),
        )?;
        match request.format {
            ImportFormat::Mcap => {
                let summary = import_mcap_files(
                    std::slice::from_ref(&validated.source_path),
                    &mut writer,
                    request.cancellation.as_ref(),
                )?;
                hints.default_timeline = Some("message_log_time".to_owned());
                hints.topics = summary.topics;
            }
            ImportFormat::Ros2BagZip => {
                let bag = rosbag::prepare_rosbag(
                    &validated.source_path,
                    request.max_output_bytes,
                    request.cancellation.as_ref(),
                )?;
                let summary =
                    import_mcap_files(&bag.mcap_paths, &mut writer, request.cancellation.as_ref())?;
                hints.default_timeline = Some("message_log_time".to_owned());
                hints.topics = summary.topics;
                hints.warnings = bag.warnings;
            }
            ImportFormat::Csv => {
                let summary = csv_import::import_csv(
                    &validated.source_path,
                    request.mapping.as_ref(),
                    &mut writer,
                )?;
                hints.default_timeline = Some(summary.default_timeline);
                hints.fps = summary.fps_hints;
                hints.topics = summary.topics;
                hints.warnings = summary.warnings;
            }
            ImportFormat::Video => {
                let summary = video::import_video(
                    &validated.source_path,
                    &request.original_file_name,
                    &validated.extension,
                    request.max_output_bytes,
                    request.cancellation.as_ref(),
                    &mut writer,
                )?;
                hints.default_timeline = Some(summary.default_timeline);
                hints.topics = summary.topics;
                hints.warnings = summary.warnings;
            }
            ImportFormat::Rrd => unreachable!("RRD passthrough is handled before encoder setup"),
        }
        writer.finish()?;
    }

    temporary.as_file_mut().flush().map_err(|err| {
        ImportError::new(
            ImportErrorCode::Io,
            "The converted recording could not be flushed.",
            format!("Failed to flush temporary RRD: {err}"),
        )
    })?;
    temporary.as_file().sync_all().map_err(|err| {
        ImportError::new(
            ImportErrorCode::Io,
            "The converted recording could not be committed to storage.",
            format!("Failed to sync temporary RRD: {err}"),
        )
    })?;
    check_cancelled(request.cancellation.as_ref())?;
    let verified = metadata::verify_rrd(
        temporary.path(),
        hints.default_timeline.as_deref(),
        &hints.fps,
        request.cancellation.as_ref(),
    )?;
    check_cancelled(request.cancellation.as_ref())?;

    let persisted = temporary
        .persist_noclobber(&validated.output_path)
        .map_err(|err| {
            ImportError::new(
                ImportErrorCode::Io,
                "The destination recording could not be committed.",
                format!(
                    "Failed to atomically persist RRD: {}\nFile path: {}",
                    err.error,
                    validated.output_path.display()
                ),
            )
        })?;
    drop(persisted);
    if let Err(err) = sync_parent_directory(output_parent) {
        re_log::warn!(
            "Committed RRD, but parent-directory durability sync was unavailable: {err}\nDirectory: {}",
            output_parent.display()
        );
        hints.warnings.push(
            "The recording was committed, but crash-durable directory sync was unavailable on this worker."
                .to_owned(),
        );
    }

    Ok(ImportResult {
        rrd_path: validated.output_path,
        source_sha256,
        content_sha256: verified.content_sha256,
        size_bytes: verified.size_bytes,
        rrd_version: verified.rrd_version,
        footer_verified: true,
        timelines: verified.timelines,
        default_timeline: verified.default_timeline,
        duration_seconds: verified.duration_seconds,
        warnings: hints.warnings,
        entity_paths: verified.entity_paths,
        entity_descriptors: verified.entity_descriptors,
        topics: hints.topics,
    })
}

/// Re-verifies a durable RRD and returns its recording entity paths.
///
/// This is intended for one-way metadata migrations of recordings that predate durable topic
/// descriptor snapshots. New imports should use [`ImportResult::entity_paths`] instead.
pub fn inspect_rrd_entity_paths(path: &Path) -> Result<Vec<String>, ImportError> {
    metadata::verify_rrd(path, None, &BTreeMap::new(), None).map(|verified| verified.entity_paths)
}

/// Re-verifies a durable RRD and returns archetype-derived visualization metadata.
///
/// This is used by one-way catalog migrations and applies the same bounded verification as a new
/// import before any descriptor is trusted.
pub fn inspect_rrd_entity_descriptors(path: &Path) -> Result<Vec<EntityDescriptor>, ImportError> {
    metadata::verify_rrd(path, None, &BTreeMap::new(), None)
        .map(|verified| verified.entity_descriptors)
}

fn sync_parent_directory(directory: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;

        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(directory)?
            .sync_all()
    }
    #[cfg(not(windows))]
    std::fs::File::open(directory)?.sync_all()
}

const MIN_OUTPUT_BYTES: u64 = 4096;
const MAX_OUTPUT_BYTES: u64 = 64 * 1024 * 1024 * 1024;
const MAX_SOURCE_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const MAX_CSV_SOURCE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_VIDEO_SOURCE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

struct ValidatedRequest {
    source_path: PathBuf,
    output_path: PathBuf,
    recording_name: String,
    extension: String,
}

#[derive(Default)]
struct ConversionHints {
    default_timeline: Option<String>,
    fps: BTreeMap<String, f64>,
    warnings: Vec<String>,
    topics: Vec<String>,
}

fn validate_request(request: &ImportRequest) -> Result<ValidatedRequest, ImportError> {
    if request.max_output_bytes < MIN_OUTPUT_BYTES || request.max_output_bytes > MAX_OUTPUT_BYTES {
        return Err(ImportError::new(
            ImportErrorCode::InvalidRequest,
            "The requested output size limit is outside the supported range.",
            format!(
                "max_output_bytes={} is outside {MIN_OUTPUT_BYTES}..={MAX_OUTPUT_BYTES}",
                request.max_output_bytes
            ),
        ));
    }
    if request.format != ImportFormat::Csv && request.mapping.is_some() {
        return Err(ImportError::new(
            ImportErrorCode::InvalidRequest,
            "A mapping can only be supplied for CSV imports.",
            format!("Mapping supplied for format {:?}", request.format),
        ));
    }
    validate_original_file_name(&request.original_file_name)?;
    let extension = Path::new(&request.original_file_name)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| {
            ImportError::new(
                ImportErrorCode::InvalidRequest,
                "The original file name must include a supported extension.",
                format!(
                    "No UTF-8 extension in originalFileName={:?}",
                    request.original_file_name
                ),
            )
        })?;
    validate_extension(request.format, &extension)?;

    let source_metadata = std::fs::symlink_metadata(&request.source_path).map_err(|err| {
        ImportError::new(
            ImportErrorCode::Io,
            "The uploaded source file is not available.",
            format!(
                "Failed to inspect source: {err}\nFile path: {}",
                request.source_path.display()
            ),
        )
    })?;
    if !source_metadata.file_type().is_file() {
        return Err(ImportError::new(
            ImportErrorCode::ValidationFailed,
            "The import source must be a regular file, not a directory or symbolic link.",
            format!(
                "Source is not a regular file: {}",
                request.source_path.display()
            ),
        ));
    }
    let source_limit = match request.format {
        ImportFormat::Csv => MAX_CSV_SOURCE_BYTES,
        ImportFormat::Video => MAX_VIDEO_SOURCE_BYTES,
        ImportFormat::Rrd | ImportFormat::Mcap | ImportFormat::Ros2BagZip => MAX_SOURCE_BYTES,
    };
    if source_metadata.len() == 0 || source_metadata.len() > source_limit {
        return Err(ImportError::new(
            ImportErrorCode::ResourceLimitExceeded,
            "The source file is empty or exceeds the import size limit.",
            format!(
                "Source size {} is outside 1..={source_limit}\nFile path: {}",
                source_metadata.len(),
                request.source_path.display()
            ),
        ));
    }
    let source_path = std::fs::canonicalize(&request.source_path).map_err(|err| {
        ImportError::new(
            ImportErrorCode::Io,
            "The uploaded source file could not be resolved.",
            format!(
                "Failed to canonicalize source: {err}\nFile path: {}",
                request.source_path.display()
            ),
        )
    })?;

    if request
        .output_rrd_path
        .extension()
        .and_then(|extension| extension.to_str())
        != Some("rrd")
    {
        return Err(ImportError::new(
            ImportErrorCode::InvalidRequest,
            "The destination file must use the .rrd extension.",
            format!(
                "Invalid output extension: {}",
                request.output_rrd_path.display()
            ),
        ));
    }
    if request.output_rrd_path.exists() {
        return Err(ImportError::new(
            ImportErrorCode::InvalidRequest,
            "The destination recording already exists.",
            format!(
                "Refusing to overwrite output: {}",
                request.output_rrd_path.display()
            ),
        ));
    }
    let output_parent = request.output_rrd_path.parent().ok_or_else(|| {
        ImportError::new(
            ImportErrorCode::InvalidRequest,
            "The destination path has no parent directory.",
            format!(
                "Output has no parent: {}",
                request.output_rrd_path.display()
            ),
        )
    })?;
    let canonical_parent = std::fs::canonicalize(output_parent).map_err(|err| {
        ImportError::new(
            ImportErrorCode::Io,
            "The destination directory is not available.",
            format!(
                "Failed to resolve output directory: {err}\nDirectory: {}",
                output_parent.display()
            ),
        )
    })?;
    if !canonical_parent.is_dir() {
        return Err(ImportError::new(
            ImportErrorCode::InvalidRequest,
            "The destination parent is not a directory.",
            format!(
                "Output parent is not a directory: {}",
                canonical_parent.display()
            ),
        ));
    }
    let output_name = request.output_rrd_path.file_name().ok_or_else(|| {
        ImportError::new(
            ImportErrorCode::InvalidRequest,
            "The destination RRD path has no file name.",
            format!(
                "Output has no file name: {}",
                request.output_rrd_path.display()
            ),
        )
    })?;
    let output_path = canonical_parent.join(output_name);
    if output_path == source_path {
        return Err(ImportError::new(
            ImportErrorCode::InvalidRequest,
            "The source and destination paths must be different.",
            format!(
                "Source and destination resolve to {}",
                source_path.display()
            ),
        ));
    }

    let recording_name = Path::new(&request.original_file_name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("rms_import")
        .to_owned();
    Ok(ValidatedRequest {
        source_path,
        output_path,
        recording_name,
        extension,
    })
}

fn validate_original_file_name(name: &str) -> Result<(), ImportError> {
    let path = Path::new(name);
    if name.is_empty()
        || name.contains(['/', '\\'])
        || path.file_name().and_then(|file_name| file_name.to_str()) != Some(name)
    {
        return Err(ImportError::new(
            ImportErrorCode::InvalidRequest,
            "The original file name must be a base name without directory components.",
            format!("Unsafe originalFileName rejected: {name:?}"),
        ));
    }
    Ok(())
}

fn validate_extension(format: ImportFormat, extension: &str) -> Result<(), ImportError> {
    let matches = match format {
        ImportFormat::Rrd => extension == "rrd",
        ImportFormat::Mcap => extension == "mcap",
        ImportFormat::Ros2BagZip => extension == "zip",
        ImportFormat::Csv => extension == "csv",
        ImportFormat::Video => matches!(extension, "mp4" | "mov" | "webm"),
    };
    if matches {
        Ok(())
    } else {
        Err(ImportError::new(
            ImportErrorCode::UnsupportedFormat,
            "The declared import format does not match the file extension.",
            format!("Format {format:?} does not accept extension {extension:?}"),
        ))
    }
}

fn copy_rrd(
    source: &Path,
    output: &mut std::fs::File,
    limit: u64,
    cancellation: Option<&ImportCancellation>,
) -> Result<(), ImportError> {
    let mut source = std::fs::File::open(source).map_err(|err| {
        ImportError::new(
            ImportErrorCode::Io,
            "The source RRD could not be read.",
            format!(
                "Failed to open source RRD: {err}\nFile path: {}",
                source.display()
            ),
        )
    })?;
    let limit_exceeded = Arc::new(AtomicBool::new(false));
    let mut output = LimitedWriter::new(output, limit, Arc::clone(&limit_exceeded));
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        check_cancelled(cancellation)?;
        let count = source.read(&mut buffer).map_err(|err| {
            ImportError::new(
                ImportErrorCode::Io,
                "The source RRD could not be read.",
                format!("Failed while copying source RRD: {err}"),
            )
        })?;
        if count == 0 {
            break;
        }
        output.write_all(&buffer[..count]).map_err(|err| {
            if limit_exceeded.load(Ordering::Acquire) {
                ImportError::new(
                    ImportErrorCode::ResourceLimitExceeded,
                    "The converted recording exceeds its output size limit.",
                    format!("RRD copy exceeded {limit} bytes"),
                )
            } else {
                ImportError::new(
                    ImportErrorCode::Io,
                    "The destination recording could not be written.",
                    format!("Failed while copying RRD: {err}"),
                )
            }
        })?;
    }
    Ok(())
}

struct McapImportSummary {
    topics: Vec<String>,
}

const MAX_MCAP_CHUNKS: usize = 65_536;
const MAX_MCAP_CHUNK_UNCOMPRESSED_BYTES: u64 = 64 * 1024 * 1024;
const MAX_MCAP_TOTAL_UNCOMPRESSED_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_MCAP_COMPRESSION_RATIO: u64 = 1000;
const MAX_MCAP_DECODE_WORKERS: usize = 2;
const MAX_MCAP_SUMMARY_BYTES: u64 = 64 * 1024 * 1024;
const MAX_MCAP_SUMMARY_RECORD_BYTES: u64 = 8 * 1024 * 1024;
const MAX_MCAP_SUMMARY_RECORDS: usize = 131_072;
const MAX_MCAP_SCHEMAS: usize = 4_096;
const MAX_MCAP_TOTAL_SCHEMA_BYTES: u64 = 32 * 1024 * 1024;
const MAX_MCAP_ATTACHMENTS: usize = 1_024;
const MAX_MCAP_ATTACHMENT_DATA_BYTES: u64 = 64 * 1024 * 1024;
const MAX_MCAP_TOTAL_ATTACHMENT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_MCAP_ATTACHMENT_OVERHEAD_BYTES: u64 = 1024 * 1024;
const MAX_MCAP_METADATA_RECORDS: usize = 4_096;
const MAX_MCAP_METADATA_RECORD_BYTES: u64 = 1024 * 1024;
const MAX_MCAP_TOTAL_METADATA_BYTES: u64 = 16 * 1024 * 1024;

static MCAP_DECODE_POOL: std::sync::LazyLock<Result<rayon::ThreadPool, String>> =
    std::sync::LazyLock::new(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(MAX_MCAP_DECODE_WORKERS)
            .thread_name(|index| format!("rms-mcap-decode-{index}"))
            .build()
            .map_err(|err| err.to_string())
    });

fn import_mcap_files(
    paths: &[PathBuf],
    writer: &mut RecordingWriter<'_>,
    cancellation: Option<&ImportCancellation>,
) -> Result<McapImportSummary, ImportError> {
    let mut topics = BTreeSet::new();
    let mut emitted_chunks = 0_usize;
    for path in paths {
        check_cancelled(cancellation)?;
        let file = std::fs::File::open(path).map_err(|err| {
            ImportError::new(
                ImportErrorCode::Io,
                "The MCAP source could not be read.",
                format!("Failed to open MCAP: {err}\nFile path: {}", path.display()),
            )
        })?;
        let metadata = file.metadata().map_err(|err| {
            ImportError::new(
                ImportErrorCode::Io,
                "The MCAP source could not be inspected.",
                format!(
                    "Failed to inspect MCAP: {err}\nFile path: {}",
                    path.display()
                ),
            )
        })?;
        if !metadata.is_file() || metadata.len() < mcap::MAGIC.len() as u64 {
            return Err(ImportError::new(
                ImportErrorCode::ValidationFailed,
                "The MCAP source is not a regular MCAP file.",
                format!("Invalid MCAP file metadata\nFile path: {}", path.display()),
            ));
        }
        // SAFETY: the import service owns immutable upload and extraction files for this call.
        #[expect(unsafe_code)]
        let mmap = unsafe { memmap2::Mmap::map(&file) }.map_err(|err| {
            ImportError::new(
                ImportErrorCode::Io,
                "The MCAP source could not be mapped for decoding.",
                format!("Failed to mmap MCAP: {err}\nFile path: {}", path.display()),
            )
        })?;
        if !mmap.starts_with(mcap::MAGIC) {
            return Err(ImportError::new(
                ImportErrorCode::ValidationFailed,
                "The declared MCAP format does not match the file contents.",
                format!("MCAP magic mismatch\nFile path: {}", path.display()),
            ));
        }
        preflight_mcap_summary(&mmap, path)?;
        let mcap_file = re_mcap::McapFile::new(mmap, false);
        let info = mcap_file.info().map_err(|err| {
            ImportError::new(
                ImportErrorCode::ValidationFailed,
                "The MCAP header or summary is invalid.",
                format!(
                    "Failed to inspect MCAP: {err}\nFile path: {}",
                    path.display()
                ),
            )
        })?;
        if info.message_count == Some(0) || info.channel_count == 0 {
            return Err(ImportError::new(
                ImportErrorCode::ValidationFailed,
                "The MCAP contains no channels or messages to Replay.",
                format!("Empty MCAP summary\nFile path: {}", path.display()),
            ));
        }
        let summary = mcap_file.summary().map_err(|err| {
            ImportError::new(
                ImportErrorCode::ValidationFailed,
                "The MCAP summary could not be validated.",
                format!(
                    "Failed to parse MCAP summary: {err}\nFile path: {}",
                    path.display()
                ),
            )
        })?;
        validate_mcap_resources(&summary, mcap_file.bytes(), writer.limit, path)?;
        topics.extend(info.channels.iter().map(|channel| channel.topic.clone()));

        let callback_error = Mutex::new(None::<ImportError>);
        let writer_lock = Mutex::new(&mut *writer);
        let chunk_counter = std::sync::atomic::AtomicUsize::new(0);
        check_cancelled(cancellation)?;
        let decode_pool = MCAP_DECODE_POOL.as_ref().map_err(|err| {
            ImportError::new(
                ImportErrorCode::ConversionFailed,
                "The MCAP decoder worker could not be started.",
                format!("Failed to build bounded MCAP decode pool: {err}"),
            )
        })?;
        let decode_result = decode_pool.install(|| {
            let should_stop = || {
                cancellation.is_some_and(ImportCancellation::is_cancelled)
                    || callback_error.lock().is_some()
            };
            re_importer::McapImporter::default()
                .with_raw_fallback(true)
                .emit_chunks_with_cancellation(
                    &mcap_file,
                    re_log_types::TimeType::TimestampNs,
                    None,
                    &should_stop,
                    &|chunk| {
                        if callback_error.lock().is_some() {
                            return;
                        }
                        if cancellation.is_some_and(ImportCancellation::is_cancelled) {
                            *callback_error.lock() = Some(ImportError::cancelled());
                            return;
                        }
                        let result = writer_lock.lock().append_chunk(&chunk);
                        match result {
                            Ok(()) => {
                                chunk_counter.fetch_add(1, Ordering::Relaxed);
                            }
                            Err(err) => {
                                *callback_error.lock() = Some(err);
                            }
                        }
                    },
                )
        });
        if let Some(error) = callback_error.into_inner() {
            return Err(error);
        }
        check_cancelled(cancellation)?;
        decode_result.map_err(|err| {
            ImportError::new(
                ImportErrorCode::ConversionFailed,
                "The MCAP could not be decoded into Rerun data.",
                format!(
                    "Rerun MCAP importer failed: {err}\nFile path: {}",
                    path.display()
                ),
            )
        })?;
        emitted_chunks += chunk_counter.load(Ordering::Relaxed);
    }
    if emitted_chunks == 0 {
        return Err(ImportError::new(
            ImportErrorCode::ValidationFailed,
            "The MCAP decoder produced no Replay data.",
            "Rerun MCAP importer emitted zero chunks",
        ));
    }
    Ok(McapImportSummary {
        topics: topics.into_iter().collect(),
    })
}

fn preflight_mcap_summary(bytes: &[u8], path: &Path) -> Result<(), ImportError> {
    const RECORD_HEADER_BYTES: usize = 9;
    const FOOTER_BODY_BYTES: usize = 20;
    const FOOTER_RECORD_BYTES: usize = RECORD_HEADER_BYTES + FOOTER_BODY_BYTES;
    const FOOTER_AND_MAGIC_BYTES: usize = FOOTER_RECORD_BYTES + mcap::MAGIC.len();

    if bytes.len() < mcap::MAGIC.len() + FOOTER_AND_MAGIC_BYTES || !bytes.ends_with(mcap::MAGIC) {
        return Err(mcap_validation_error(
            "MCAP footer or trailing magic is missing",
            path,
        ));
    }
    let footer_start = bytes.len() - FOOTER_AND_MAGIC_BYTES;
    let footer_header = bytes
        .get(footer_start..footer_start + RECORD_HEADER_BYTES)
        .ok_or_else(|| mcap_validation_error("MCAP footer header is truncated", path))?;
    if footer_header[0] != mcap::records::op::FOOTER {
        return Err(mcap_validation_error(
            "MCAP final record is not a footer",
            path,
        ));
    }
    let footer_body_size = u64::from_le_bytes(footer_header[1..9].try_into().map_err(|err| {
        mcap_validation_error(format!("MCAP footer length parse failed: {err}"), path)
    })?);
    if footer_body_size != FOOTER_BODY_BYTES as u64 {
        return Err(mcap_validation_error(
            format!("MCAP footer body is {footer_body_size} bytes; expected {FOOTER_BODY_BYTES}"),
            path,
        ));
    }
    let footer_body = bytes
        .get(footer_start + RECORD_HEADER_BYTES..footer_start + FOOTER_RECORD_BYTES)
        .ok_or_else(|| mcap_validation_error("MCAP footer body is truncated", path))?;
    let summary_start = usize::try_from(read_u64_le(footer_body, 0, path)?).map_err(|err| {
        mcap_resource_error(
            format!("MCAP summary offset does not fit memory: {err}"),
            path,
        )
    })?;
    let summary_offset_start =
        usize::try_from(read_u64_le(footer_body, 8, path)?).map_err(|err| {
            mcap_resource_error(
                format!("MCAP summary-offset position does not fit memory: {err}"),
                path,
            )
        })?;
    if summary_start < mcap::MAGIC.len() || summary_start >= footer_start {
        return Err(mcap_validation_error(
            format!(
                "MCAP summary start {summary_start} is outside {}..{footer_start}",
                mcap::MAGIC.len()
            ),
            path,
        ));
    }
    if summary_offset_start != 0
        && (summary_offset_start < summary_start || summary_offset_start >= footer_start)
    {
        return Err(mcap_validation_error(
            format!(
                "MCAP summary-offset start {summary_offset_start} is outside {summary_start}..{footer_start}"
            ),
            path,
        ));
    }
    let summary_size = footer_start - summary_start;
    if summary_size as u64 > MAX_MCAP_SUMMARY_BYTES {
        return Err(mcap_resource_error(
            format!("MCAP summary is {summary_size} bytes; maximum is {MAX_MCAP_SUMMARY_BYTES}"),
            path,
        ));
    }

    let mut cursor = summary_start;
    let mut record_count = 0_usize;
    let mut chunk_index_count = 0_usize;
    let mut schema_count = 0_usize;
    let mut schema_bytes = 0_u64;
    let mut attachment_index_count = 0_usize;
    let mut metadata_index_count = 0_usize;
    while cursor < footer_start {
        let header_end = cursor
            .checked_add(RECORD_HEADER_BYTES)
            .ok_or_else(|| mcap_validation_error("MCAP summary header overflow", path))?;
        let header = bytes.get(cursor..header_end).ok_or_else(|| {
            mcap_validation_error("MCAP summary record header is truncated", path)
        })?;
        let opcode = header[0];
        if !matches!(
            opcode,
            mcap::records::op::SCHEMA
                | mcap::records::op::CHANNEL
                | mcap::records::op::CHUNK_INDEX
                | mcap::records::op::ATTACHMENT_INDEX
                | mcap::records::op::STATISTICS
                | mcap::records::op::METADATA_INDEX
                | mcap::records::op::SUMMARY_OFFSET
        ) {
            return Err(mcap_validation_error(
                format!("MCAP summary contains non-summary opcode 0x{opcode:02x}"),
                path,
            ));
        }
        let body_size = u64::from_le_bytes(header[1..9].try_into().map_err(|err| {
            mcap_validation_error(format!("MCAP summary length parse failed: {err}"), path)
        })?);
        if body_size > MAX_MCAP_SUMMARY_RECORD_BYTES {
            return Err(mcap_resource_error(
                format!(
                    "MCAP summary record declares {body_size} bytes; maximum is {MAX_MCAP_SUMMARY_RECORD_BYTES}"
                ),
                path,
            ));
        }
        let body_size = usize::try_from(body_size).map_err(|err| {
            mcap_resource_error(
                format!("MCAP summary record size does not fit memory: {err}"),
                path,
            )
        })?;
        cursor = header_end
            .checked_add(body_size)
            .ok_or_else(|| mcap_validation_error("MCAP summary record span overflow", path))?;
        if cursor > footer_start {
            return Err(mcap_validation_error(
                "MCAP summary record extends into the footer",
                path,
            ));
        }
        record_count += 1;
        if record_count > MAX_MCAP_SUMMARY_RECORDS {
            return Err(mcap_resource_error(
                format!("MCAP summary has more than {MAX_MCAP_SUMMARY_RECORDS} records"),
                path,
            ));
        }
        if opcode == mcap::records::op::CHUNK_INDEX {
            chunk_index_count += 1;
            if chunk_index_count > MAX_MCAP_CHUNKS {
                return Err(mcap_resource_error(
                    format!("MCAP summary has more than {MAX_MCAP_CHUNKS} chunk indexes"),
                    path,
                ));
            }
        } else if opcode == mcap::records::op::SCHEMA {
            schema_count += 1;
            schema_bytes = schema_bytes
                .checked_add(body_size as u64)
                .ok_or_else(|| mcap_resource_error("MCAP schema byte count overflow", path))?;
            if schema_count > MAX_MCAP_SCHEMAS || schema_bytes > MAX_MCAP_TOTAL_SCHEMA_BYTES {
                return Err(mcap_resource_error(
                    format!(
                        "MCAP summary schema budget exceeds {MAX_MCAP_SCHEMAS} records or {MAX_MCAP_TOTAL_SCHEMA_BYTES} bytes"
                    ),
                    path,
                ));
            }
        } else if opcode == mcap::records::op::ATTACHMENT_INDEX {
            attachment_index_count += 1;
            if attachment_index_count > MAX_MCAP_ATTACHMENTS {
                return Err(mcap_resource_error(
                    format!("MCAP has more than {MAX_MCAP_ATTACHMENTS} attachment indexes"),
                    path,
                ));
            }
        } else if opcode == mcap::records::op::METADATA_INDEX {
            metadata_index_count += 1;
            if metadata_index_count > MAX_MCAP_METADATA_RECORDS {
                return Err(mcap_resource_error(
                    format!("MCAP has more than {MAX_MCAP_METADATA_RECORDS} metadata indexes"),
                    path,
                ));
            }
        }
    }
    if chunk_index_count == 0 {
        return Err(mcap_validation_error(
            "MCAP summary contains no chunk indexes",
            path,
        ));
    }
    Ok(())
}

fn validate_mcap_resources(
    summary: &re_mcap::Summary,
    bytes: &[u8],
    output_limit: u64,
    path: &Path,
) -> Result<(), ImportError> {
    if summary.chunk_indexes.len() > MAX_MCAP_CHUNKS {
        return Err(mcap_resource_error(
            format!(
                "MCAP has {} chunks; maximum is {MAX_MCAP_CHUNKS}",
                summary.chunk_indexes.len()
            ),
            path,
        ));
    }
    if summary.schemas.len() > MAX_MCAP_SCHEMAS {
        return Err(mcap_resource_error(
            format!("MCAP has more than {MAX_MCAP_SCHEMAS} schemas"),
            path,
        ));
    }
    let total_schema_bytes = summary.schemas.values().try_fold(0_u64, |total, schema| {
        let size = u64::try_from(schema.data.len()).map_err(|err| {
            mcap_resource_error(format!("MCAP schema size overflow: {err}"), path)
        })?;
        if size > MAX_MCAP_SUMMARY_RECORD_BYTES {
            return Err(mcap_resource_error(
                format!("MCAP schema is {size} bytes; maximum is {MAX_MCAP_SUMMARY_RECORD_BYTES}"),
                path,
            ));
        }
        total
            .checked_add(size)
            .ok_or_else(|| mcap_resource_error("MCAP schema byte count overflow", path))
    })?;
    if total_schema_bytes > MAX_MCAP_TOTAL_SCHEMA_BYTES {
        return Err(mcap_resource_error(
            format!(
                "MCAP schemas contain {total_schema_bytes} bytes; maximum is {MAX_MCAP_TOTAL_SCHEMA_BYTES}"
            ),
            path,
        ));
    }
    if summary.attachment_indexes.len() > MAX_MCAP_ATTACHMENTS {
        return Err(mcap_resource_error(
            format!("MCAP has more than {MAX_MCAP_ATTACHMENTS} attachments"),
            path,
        ));
    }
    if summary.metadata_indexes.len() > MAX_MCAP_METADATA_RECORDS {
        return Err(mcap_resource_error(
            format!("MCAP has more than {MAX_MCAP_METADATA_RECORDS} metadata records"),
            path,
        ));
    }
    let total_limit = output_limit
        .saturating_mul(32)
        .clamp(64 * 1024 * 1024, MAX_MCAP_TOTAL_UNCOMPRESSED_BYTES);
    let mut total_uncompressed = 0_u64;
    let mut spans = Vec::with_capacity(summary.chunk_indexes.len());
    for index in &summary.chunk_indexes {
        let actual = inspect_mcap_chunk(bytes, index, path)?;
        if actual.uncompressed_size > MAX_MCAP_CHUNK_UNCOMPRESSED_BYTES
            || actual.uncompressed_size > total_limit
        {
            return Err(mcap_resource_error(
                format!(
                    "MCAP chunk declares {} uncompressed bytes; per-chunk limit is {}",
                    actual.uncompressed_size,
                    MAX_MCAP_CHUNK_UNCOMPRESSED_BYTES.min(total_limit)
                ),
                path,
            ));
        }
        total_uncompressed = total_uncompressed
            .checked_add(actual.uncompressed_size)
            .ok_or_else(|| mcap_resource_error("MCAP uncompressed size overflow", path))?;
        if actual.compressed_size == 0
            || actual.uncompressed_size
                > actual
                    .compressed_size
                    .saturating_mul(MAX_MCAP_COMPRESSION_RATIO)
        {
            return Err(mcap_resource_error(
                format!(
                    "MCAP codec {:?} exceeds compression ratio {MAX_MCAP_COMPRESSION_RATIO}:1",
                    actual.compression
                ),
                path,
            ));
        }
        spans.push((index.chunk_start_offset, actual.record_end));
    }
    if total_uncompressed > total_limit {
        return Err(mcap_resource_error(
            format!(
                "MCAP declares {total_uncompressed} uncompressed bytes; request-linked limit is {total_limit}"
            ),
            path,
        ));
    }

    let attachment_total_limit = output_limit
        .saturating_mul(4)
        .clamp(8 * 1024 * 1024, MAX_MCAP_TOTAL_ATTACHMENT_BYTES);
    let mut total_attachment_bytes = 0_u64;
    for index in &summary.attachment_indexes {
        if index.data_size > MAX_MCAP_ATTACHMENT_DATA_BYTES {
            return Err(mcap_resource_error(
                format!(
                    "MCAP attachment declares {} data bytes; maximum is {MAX_MCAP_ATTACHMENT_DATA_BYTES}",
                    index.data_size
                ),
                path,
            ));
        }
        total_attachment_bytes = total_attachment_bytes
            .checked_add(index.data_size)
            .ok_or_else(|| mcap_resource_error("MCAP attachment byte count overflow", path))?;
        if total_attachment_bytes > attachment_total_limit {
            return Err(mcap_resource_error(
                format!(
                    "MCAP attachments contain {total_attachment_bytes} bytes; request-linked maximum is {attachment_total_limit}"
                ),
                path,
            ));
        }
        let max_record_bytes = MAX_MCAP_ATTACHMENT_DATA_BYTES
            .checked_add(MAX_MCAP_ATTACHMENT_OVERHEAD_BYTES)
            .expect("attachment limits fit u64");
        let span = inspect_mcap_indexed_record(
            bytes,
            index.offset,
            index.length,
            mcap::records::op::ATTACHMENT,
            "attachment",
            max_record_bytes,
            path,
        )?;
        let attachment = mcap::read::attachment(bytes, index).map_err(|err| {
            mcap_validation_error(format!("MCAP attachment index is invalid: {err}"), path)
        })?;
        if u64::try_from(attachment.data.len()).ok() != Some(index.data_size)
            || attachment.log_time != index.log_time
            || attachment.create_time != index.create_time
            || attachment.name != index.name
            || attachment.media_type != index.media_type
        {
            return Err(mcap_validation_error(
                "MCAP attachment record disagrees with its index",
                path,
            ));
        }
        spans.push(span);
    }

    let mut total_metadata_bytes = 0_u64;
    for index in &summary.metadata_indexes {
        total_metadata_bytes = total_metadata_bytes
            .checked_add(index.length)
            .ok_or_else(|| mcap_resource_error("MCAP metadata byte count overflow", path))?;
        if total_metadata_bytes > MAX_MCAP_TOTAL_METADATA_BYTES {
            return Err(mcap_resource_error(
                format!(
                    "MCAP metadata records contain {total_metadata_bytes} bytes; maximum is {MAX_MCAP_TOTAL_METADATA_BYTES}"
                ),
                path,
            ));
        }
        let span = inspect_mcap_indexed_record(
            bytes,
            index.offset,
            index.length,
            mcap::records::op::METADATA,
            "metadata",
            MAX_MCAP_METADATA_RECORD_BYTES,
            path,
        )?;
        let metadata = mcap::read::metadata(bytes, index).map_err(|err| {
            mcap_validation_error(format!("MCAP metadata index is invalid: {err}"), path)
        })?;
        if metadata.name != index.name {
            return Err(mcap_validation_error(
                "MCAP metadata record disagrees with its index",
                path,
            ));
        }
        spans.push(span);
    }

    spans.sort_unstable();
    for adjacent in spans.windows(2) {
        if adjacent[1].0 < adjacent[0].1 {
            return Err(ImportError::new(
                ImportErrorCode::ValidationFailed,
                "The MCAP chunk indexes contain overlapping byte spans.",
                format!(
                    "MCAP chunk spans overlap: {:?} and {:?}\nFile path: {}",
                    adjacent[0],
                    adjacent[1],
                    path.display()
                ),
            ));
        }
    }
    Ok(())
}

fn inspect_mcap_indexed_record(
    bytes: &[u8],
    offset: u64,
    length: u64,
    expected_opcode: u8,
    label: &str,
    max_length: u64,
    path: &Path,
) -> Result<(u64, u64), ImportError> {
    const RECORD_HEADER_BYTES: u64 = 9;
    if length < RECORD_HEADER_BYTES || length > max_length {
        return Err(mcap_resource_error(
            format!("MCAP {label} record is {length} bytes; maximum is {max_length}"),
            path,
        ));
    }
    let header = mcap_slice(bytes, offset, RECORD_HEADER_BYTES, path)?;
    if header[0] != expected_opcode {
        return Err(mcap_validation_error(
            format!(
                "MCAP {label} index points to opcode 0x{:02x}, expected 0x{expected_opcode:02x}",
                header[0]
            ),
            path,
        ));
    }
    let body_size = u64::from_le_bytes(header[1..9].try_into().map_err(|err| {
        mcap_validation_error(
            format!("MCAP {label} record length parse failed: {err}"),
            path,
        )
    })?);
    let actual_length = RECORD_HEADER_BYTES
        .checked_add(body_size)
        .ok_or_else(|| mcap_validation_error(format!("MCAP {label} length overflow"), path))?;
    if actual_length != length {
        return Err(mcap_validation_error(
            format!("MCAP {label} record length {actual_length} disagrees with index {length}"),
            path,
        ));
    }
    let end = offset
        .checked_add(length)
        .ok_or_else(|| mcap_validation_error(format!("MCAP {label} end overflow"), path))?;
    mcap_slice(bytes, offset, length, path)?;
    Ok((offset, end))
}

struct InspectedMcapChunk {
    record_end: u64,
    compression: String,
    compressed_size: u64,
    uncompressed_size: u64,
}

fn inspect_mcap_chunk(
    bytes: &[u8],
    index: &mcap::records::ChunkIndex,
    path: &Path,
) -> Result<InspectedMcapChunk, ImportError> {
    const RECORD_HEADER_BYTES: u64 = 9;
    const FIXED_CHUNK_HEADER_BYTES: u64 = 8 + 8 + 8 + 4 + 4;
    const MAX_COMPRESSION_NAME_BYTES: u64 = 64;

    let record_start = index.chunk_start_offset;
    let body_start = record_start
        .checked_add(RECORD_HEADER_BYTES)
        .ok_or_else(|| mcap_validation_error("MCAP chunk record offset overflow", path))?;
    let record_prefix = mcap_slice(bytes, record_start, RECORD_HEADER_BYTES, path)?;
    if record_prefix[0] != mcap::records::op::CHUNK {
        return Err(mcap_validation_error(
            format!(
                "MCAP chunk index points to opcode 0x{:02x}, not CHUNK",
                record_prefix[0]
            ),
            path,
        ));
    }
    let body_size = u64::from_le_bytes(record_prefix[1..9].try_into().map_err(|err| {
        mcap_validation_error(format!("MCAP record length parse failed: {err}"), path)
    })?);
    let record_size = RECORD_HEADER_BYTES
        .checked_add(body_size)
        .ok_or_else(|| mcap_validation_error("MCAP chunk record length overflow", path))?;
    if record_size != index.chunk_length {
        return Err(mcap_validation_error(
            format!(
                "MCAP chunk record length {record_size} disagrees with index {}",
                index.chunk_length
            ),
            path,
        ));
    }
    let record_end = record_start
        .checked_add(record_size)
        .ok_or_else(|| mcap_validation_error("MCAP chunk end offset overflow", path))?;
    let fixed = mcap_slice(bytes, body_start, FIXED_CHUNK_HEADER_BYTES, path)?;
    let message_start_time = read_u64_le(fixed, 0, path)?;
    let message_end_time = read_u64_le(fixed, 8, path)?;
    let uncompressed_size = read_u64_le(fixed, 16, path)?;
    let compression_len = u64::from(u32::from_le_bytes(fixed[28..32].try_into().map_err(
        |err| mcap_validation_error(format!("MCAP compression length parse failed: {err}"), path),
    )?));
    if compression_len > MAX_COMPRESSION_NAME_BYTES {
        return Err(mcap_validation_error(
            format!("MCAP compression name is {compression_len} bytes; maximum is 64"),
            path,
        ));
    }
    let compression_start = body_start
        .checked_add(FIXED_CHUNK_HEADER_BYTES)
        .ok_or_else(|| mcap_validation_error("MCAP compression offset overflow", path))?;
    let compression_bytes = mcap_slice(bytes, compression_start, compression_len, path)?;
    let compression = std::str::from_utf8(compression_bytes)
        .map_err(|err| {
            mcap_validation_error(format!("MCAP compression is not UTF-8: {err}"), path)
        })?
        .to_owned();
    let compressed_size_offset = compression_start
        .checked_add(compression_len)
        .ok_or_else(|| mcap_validation_error("MCAP compressed-size offset overflow", path))?;
    let compressed_size =
        read_u64_le(mcap_slice(bytes, compressed_size_offset, 8, path)?, 0, path)?;
    let data_start = compressed_size_offset
        .checked_add(8)
        .ok_or_else(|| mcap_validation_error("MCAP chunk data offset overflow", path))?;
    let data_end = data_start
        .checked_add(compressed_size)
        .ok_or_else(|| mcap_validation_error("MCAP chunk data span overflow", path))?;
    if data_end != record_end {
        return Err(mcap_validation_error(
            format!(
                "MCAP chunk payload ends at {data_end}, but indexed record ends at {record_end}"
            ),
            path,
        ));
    }
    mcap_slice(bytes, data_start, compressed_size, path)?;
    if message_start_time != index.message_start_time
        || message_end_time != index.message_end_time
        || uncompressed_size != index.uncompressed_size
        || compressed_size != index.compressed_size
        || compression != index.compression
    {
        return Err(mcap_validation_error(
            format!(
                "MCAP chunk header disagrees with index: actual=({message_start_time},{message_end_time},{uncompressed_size},{compressed_size},{compression:?}), index=({},{},{},{},{:?})",
                index.message_start_time,
                index.message_end_time,
                index.uncompressed_size,
                index.compressed_size,
                index.compression
            ),
            path,
        ));
    }
    Ok(InspectedMcapChunk {
        record_end,
        compression,
        compressed_size,
        uncompressed_size,
    })
}

fn mcap_slice<'a>(
    bytes: &'a [u8],
    offset: u64,
    len: u64,
    path: &Path,
) -> Result<&'a [u8], ImportError> {
    let end = offset
        .checked_add(len)
        .ok_or_else(|| mcap_validation_error("MCAP byte span overflow", path))?;
    let offset = usize::try_from(offset).map_err(|err| {
        mcap_validation_error(format!("MCAP byte offset does not fit memory: {err}"), path)
    })?;
    let end = usize::try_from(end).map_err(|err| {
        mcap_validation_error(format!("MCAP byte end does not fit memory: {err}"), path)
    })?;
    bytes.get(offset..end).ok_or_else(|| {
        mcap_validation_error(
            format!(
                "MCAP byte span {offset}..{end} exceeds file size {}",
                bytes.len()
            ),
            path,
        )
    })
}

fn read_u64_le(bytes: &[u8], offset: usize, path: &Path) -> Result<u64, ImportError> {
    let end = offset
        .checked_add(8)
        .ok_or_else(|| mcap_validation_error("MCAP integer offset overflow", path))?;
    let bytes = bytes
        .get(offset..end)
        .ok_or_else(|| mcap_validation_error("MCAP chunk header is truncated", path))?;
    Ok(u64::from_le_bytes(bytes.try_into().map_err(|err| {
        mcap_validation_error(format!("MCAP integer parse failed: {err}"), path)
    })?))
}

fn mcap_validation_error(detail: impl Into<String>, path: &Path) -> ImportError {
    ImportError::new(
        ImportErrorCode::ValidationFailed,
        "The MCAP chunk index or payload framing is corrupt.",
        format!("{}\nFile path: {}", detail.into(), path.display()),
    )
}

fn mcap_resource_error(detail: impl Into<String>, path: &Path) -> ImportError {
    ImportError::new(
        ImportErrorCode::ResourceLimitExceeded,
        "The MCAP exceeds the safe decompression limits.",
        format!("{}\nFile path: {}", detail.into(), path.display()),
    )
}

struct RecordingWriter<'a> {
    encoder: re_log_encoding::Encoder<LimitedWriter<'a>>,
    store_id: re_log_types::StoreId,
    limit: u64,
    limit_exceeded: Arc<AtomicBool>,
    cancellation: Option<ImportCancellation>,
    chunk_count: usize,
}

impl<'a> RecordingWriter<'a> {
    fn check_cancelled(&self) -> Result<(), ImportError> {
        check_cancelled(self.cancellation.as_ref())
    }

    fn new(
        output: &'a mut std::fs::File,
        limit: u64,
        recording_name: &str,
        cancellation: Option<ImportCancellation>,
    ) -> Result<Self, ImportError> {
        let application_id = re_log_types::ApplicationId::try_new("rms_import").map_err(|err| {
            ImportError::new(
                ImportErrorCode::ConversionFailed,
                "The import recording identity could not be created.",
                format!("Failed to create RMS import application ID: {err}"),
            )
        })?;
        let store_id = re_log_types::StoreId::recording(
            application_id,
            re_log_types::RecordingId::from(recording_name.to_owned()),
        );
        let limit_exceeded = Arc::new(AtomicBool::new(false));
        let output = LimitedWriter::new(output, limit, Arc::clone(&limit_exceeded));
        let encoder = re_log_encoding::Encoder::new_eager(
            re_build_info::CrateVersion::LOCAL,
            re_log_encoding::EncodingOptions::PROTOBUF_COMPRESSED,
            output,
        )
        .map_err(|err| encode_error(&err, limit_exceeded.load(Ordering::Acquire), limit))?;
        let mut writer = Self {
            encoder,
            store_id,
            limit,
            limit_exceeded,
            cancellation,
            chunk_count: 0,
        };
        let store_info = re_log_types::LogMsg::SetStoreInfo(re_log_types::SetStoreInfo {
            row_id: *re_chunk::RowId::new(),
            info: re_log_types::StoreInfo::new(
                writer.store_id.clone(),
                re_log_types::StoreSource::Unknown,
            ),
        });
        writer.append_log_msg(&store_info)?;
        Ok(writer)
    }

    fn append_chunk(&mut self, chunk: &re_chunk::Chunk) -> Result<(), ImportError> {
        check_cancelled(self.cancellation.as_ref())?;
        let arrow_msg = chunk.to_arrow_msg().map_err(|err| {
            ImportError::new(
                ImportErrorCode::ConversionFailed,
                "Imported data could not be serialized as an RRD chunk.",
                format!("Failed to serialize imported chunk: {err}"),
            )
        })?;
        self.append_log_msg(&re_log_types::LogMsg::ArrowMsg(
            self.store_id.clone(),
            arrow_msg,
        ))?;
        self.chunk_count += 1;
        Ok(())
    }

    fn append_log_msg(&mut self, message: &re_log_types::LogMsg) -> Result<(), ImportError> {
        self.encoder.append(message).map(|_| ()).map_err(|err| {
            encode_error(
                &err,
                self.limit_exceeded.load(Ordering::Acquire),
                self.limit,
            )
        })
    }

    fn finish(mut self) -> Result<(), ImportError> {
        check_cancelled(self.cancellation.as_ref())?;
        if self.chunk_count == 0 {
            return Err(ImportError::new(
                ImportErrorCode::ValidationFailed,
                "The importer produced no Replay data.",
                "Recording writer received zero chunks",
            ));
        }
        let limit = self.limit;
        self.encoder.finish().map_err(|err| {
            encode_error(&err, self.limit_exceeded.load(Ordering::Acquire), limit)
        })?;
        self.encoder
            .flush_blocking()
            .map_err(|err| encode_error(&err, self.limit_exceeded.load(Ordering::Acquire), limit))
    }
}

struct LimitedWriter<'a> {
    inner: &'a mut std::fs::File,
    limit: u64,
    written: u64,
    limit_exceeded: Arc<AtomicBool>,
}

impl<'a> LimitedWriter<'a> {
    fn new(inner: &'a mut std::fs::File, limit: u64, limit_exceeded: Arc<AtomicBool>) -> Self {
        Self {
            inner,
            limit,
            written: 0,
            limit_exceeded,
        }
    }
}

impl std::io::Write for LimitedWriter<'_> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let requested = u64::try_from(buffer.len()).unwrap_or(u64::MAX);
        if self
            .written
            .checked_add(requested)
            .is_none_or(|size| size > self.limit)
        {
            self.limit_exceeded.store(true, Ordering::Release);
            return Err(std::io::Error::new(
                std::io::ErrorKind::FileTooLarge,
                "RMS_IMPORT_OUTPUT_LIMIT",
            ));
        }
        let written = self.inner.write(buffer)?;
        self.written += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn encode_error(
    err: &re_log_encoding::EncodeError,
    limit_exceeded: bool,
    limit: u64,
) -> ImportError {
    if limit_exceeded {
        ImportError::new(
            ImportErrorCode::ResourceLimitExceeded,
            "The converted recording exceeds its output size limit.",
            format!("RRD encoder exceeded {limit} bytes: {err}"),
        )
    } else {
        ImportError::new(
            ImportErrorCode::ConversionFailed,
            "Imported data could not be encoded as RRD.",
            format!("RRD encoder failed: {err}"),
        )
    }
}

fn check_cancelled(cancellation: Option<&ImportCancellation>) -> Result<(), ImportError> {
    if cancellation.is_some_and(ImportCancellation::is_cancelled) {
        Err(ImportError::cancelled())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::collections::BTreeMap;
    use std::io::Write as _;
    use std::path::{Path, PathBuf};

    use super::{
        ImportCancellation, ImportErrorCode, ImportFormat, ImportRequest,
        MAX_MCAP_ATTACHMENT_DATA_BYTES, MAX_MCAP_ATTACHMENTS, MAX_MCAP_CHUNK_UNCOMPRESSED_BYTES,
        MAX_MCAP_CHUNKS, MAX_MCAP_DECODE_WORKERS, MAX_MCAP_SUMMARY_RECORDS,
        MAX_MCAP_TOTAL_ATTACHMENT_BYTES, MAX_MCAP_TOTAL_METADATA_BYTES,
        MAX_MCAP_TOTAL_UNCOMPRESSED_BYTES, MCAP_DECODE_POOL, TimelineKind, preflight_mcap_summary,
        run_import, validate_mcap_resources,
    };

    const TEST_OUTPUT_LIMIT: u64 = 128 * 1024 * 1024;

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
    }

    fn request(source: &Path, output: &Path, name: &str, format: ImportFormat) -> ImportRequest {
        ImportRequest {
            source_path: source.to_owned(),
            output_rrd_path: output.to_owned(),
            original_file_name: name.to_owned(),
            format,
            mapping: None,
            max_output_bytes: TEST_OUTPUT_LIMIT,
            cancellation: None,
        }
    }

    #[test]
    fn csv_default_and_duration_mapping_produce_verified_rrd()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let source = temp.path().join("telemetry.csv");
        std::fs::write(
            &source,
            "elapsed,value,label\n0,1,boot\n0.5,2,run\n1.5,3,done\n",
        )?;

        let default_output = temp.path().join("default.rrd");
        let result = run_import(&request(
            &source,
            &default_output,
            "telemetry.csv",
            ImportFormat::Csv,
        ))?;
        let row = result
            .timelines
            .iter()
            .find(|timeline| timeline.name == "row")
            .ok_or("row timeline missing")?;
        assert_eq!(row.kind, TimelineKind::Sequence);
        assert_eq!(row.fps, Some(1.0));
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.contains("1 FPS"))
        );
        assert!(result.footer_verified);

        let duration_output = temp.path().join("duration.rrd");
        let mut duration_request = request(
            &source,
            &duration_output,
            "telemetry.csv",
            ImportFormat::Csv,
        );
        duration_request.mapping = Some(serde_json::json!({
            "timeline": {
                "column": "elapsed",
                "name": "elapsed",
                "kind": "duration",
                "unit": "s"
            },
            "columns": [{"column": "value", "entityPath": "value"}]
        }));
        let duration = run_import(&duration_request)?;
        let timeline = duration
            .timelines
            .iter()
            .find(|timeline| timeline.name == "elapsed")
            .ok_or("duration timeline missing")?;
        assert_eq!(timeline.kind, TimelineKind::Duration);
        assert_eq!(timeline.start, "0");
        assert_eq!(timeline.end, "1500000000");
        assert_eq!(timeline.duration_seconds, Some(1.5));

        let passthrough_output = temp.path().join("passthrough.rrd");
        let passthrough = run_import(&request(
            &duration_output,
            &passthrough_output,
            "duration.rrd",
            ImportFormat::Rrd,
        ))?;
        assert_eq!(passthrough.content_sha256, duration.content_sha256);
        assert_eq!(passthrough.timelines, duration.timelines);
        Ok(())
    }

    #[test]
    fn real_mcap_and_mp4_fixtures_convert_to_verified_rrd() -> Result<(), Box<dyn std::error::Error>>
    {
        let root = workspace_root();
        let temp = tempfile::tempdir()?;
        let mcap = root.join("crates/store/re_importer/tests/assets/supported_ros2_messages.mcap");
        let mcap_result = run_import(&request(
            &mcap,
            &temp.path().join("messages.rrd"),
            "messages.mcap",
            ImportFormat::Mcap,
        ))?;
        assert!(mcap_result.footer_verified);
        assert!(!mcap_result.topics.is_empty());
        assert!(!mcap_result.entity_paths.is_empty());

        let mp4 = root.join("tests/assets/video/Big_Buck_Bunny_1080_1s_h264_nobframes.mp4");
        let video_result = run_import(&request(
            &mp4,
            &temp.path().join("video.rrd"),
            "video.mp4",
            ImportFormat::Video,
        ))?;
        assert!(video_result.footer_verified);
        assert_eq!(video_result.default_timeline, "video");
        assert!(
            video_result
                .timelines
                .iter()
                .any(|timeline| timeline.kind == TimelineKind::Duration)
        );
        Ok(())
    }

    #[test]
    fn rosbag_zip_uses_only_metadata_declared_real_mcap_segments()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = workspace_root();
        let segment =
            root.join("crates/store/re_importer/tests/assets/supported_ros2_messages.mcap");
        let temp = tempfile::tempdir()?;
        let archive_path = temp.path().join("bag.zip");
        write_rosbag_zip(&archive_path, &segment, false)?;

        let result = run_import(&request(
            &archive_path,
            &temp.path().join("bag.rrd"),
            "bag.zip",
            ImportFormat::Ros2BagZip,
        ))?;
        assert!(result.footer_verified);
        assert!(!result.topics.is_empty());

        let injected_path = temp.path().join("injected.zip");
        write_rosbag_zip(&injected_path, &segment, true)?;
        let error = run_import(&request(
            &injected_path,
            &temp.path().join("injected.rrd"),
            "injected.zip",
            ImportFormat::Ros2BagZip,
        ))
        .expect_err("unlisted MCAP segment must be rejected");
        assert_eq!(error.code(), ImportErrorCode::ValidationFailed);
        Ok(())
    }

    #[test]
    fn installed_tools_convert_real_webm_and_sqlite_rosbag()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = workspace_root();
        let temp = tempfile::tempdir()?;
        let mp4 = root.join("tests/assets/video/Big_Buck_Bunny_1080_1s_h264_nobframes.mp4");
        let webm = temp.path().join("fixture.webm");
        super::external_tools::transcode_fixture_to_webm(&mp4, &webm)?;
        let webm_result = run_import(&request(
            &webm,
            &temp.path().join("webm.rrd"),
            "fixture.webm",
            ImportFormat::Video,
        ))?;
        assert!(webm_result.footer_verified);
        assert!(
            webm_result
                .warnings
                .iter()
                .any(|warning| warning.contains("Transcoded webm"))
        );

        let mcap = root.join("crates/store/re_importer/tests/assets/supported_ros2_messages.mcap");
        let sqlite_bag = temp.path().join("sqlite_bag");
        super::external_tools::convert_fixture_mcap_to_db3(&mcap, &sqlite_bag)?;
        let archive = temp.path().join("sqlite_bag.zip");
        write_directory_zip(&archive, &sqlite_bag, "bag")?;
        let db3_result = run_import(&request(
            &archive,
            &temp.path().join("sqlite_bag.rrd"),
            "sqlite_bag.zip",
            ImportFormat::Ros2BagZip,
        ))?;
        assert!(db3_result.footer_verified);
        assert!(
            db3_result
                .warnings
                .iter()
                .any(|warning| warning.contains("sqlite3"))
        );
        Ok(())
    }

    fn write_rosbag_zip(
        archive_path: &Path,
        segment: &Path,
        include_unlisted: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let file = std::fs::File::create(archive_path)?;
        let mut archive = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        archive.start_file("bag/metadata.yaml", options)?;
        archive.write_all(
            b"rosbag2_bagfile_information:\n  version: 9\n  storage_identifier: mcap\n  relative_file_paths:\n    - data.mcap\n",
        )?;
        archive.start_file("bag/data.mcap", options)?;
        std::io::copy(&mut std::fs::File::open(segment)?, &mut archive)?;
        if include_unlisted {
            archive.start_file("bag/hidden.mcap", options)?;
            std::io::copy(&mut std::fs::File::open(segment)?, &mut archive)?;
        }
        archive.finish()?;
        Ok(())
    }

    fn write_directory_zip(
        archive_path: &Path,
        source_directory: &Path,
        prefix: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let file = std::fs::File::create(archive_path)?;
        let mut archive = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        let mut files = Vec::new();
        collect_regular_files(source_directory, &mut files)?;
        files.sort();
        for path in files {
            let relative = path.strip_prefix(source_directory)?;
            let relative = relative
                .components()
                .map(|component| component.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            archive.start_file(format!("{prefix}/{relative}"), options)?;
            std::io::copy(&mut std::fs::File::open(path)?, &mut archive)?;
        }
        archive.finish()?;
        Ok(())
    }

    fn collect_regular_files(
        directory: &Path,
        files: &mut Vec<PathBuf>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                collect_regular_files(&entry.path(), files)?;
            } else if file_type.is_file() {
                files.push(entry.path());
            } else {
                return Err(format!(
                    "unsupported fixture filesystem entry: {}",
                    entry.path().display()
                )
                .into());
            }
        }
        Ok(())
    }

    #[test]
    fn cancellation_is_fail_closed_before_destination_creation()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let source = temp.path().join("data.csv");
        let output = temp.path().join("cancelled.rrd");
        std::fs::write(&source, "value\n1\n")?;
        let cancellation = ImportCancellation::new();
        cancellation.cancel();
        let mut request = request(&source, &output, "data.csv", ImportFormat::Csv);
        request.cancellation = Some(cancellation);
        let error = run_import(&request).expect_err("cancelled import must fail");
        assert_eq!(error.code(), ImportErrorCode::Cancelled);
        assert!(!output.exists());
        Ok(())
    }

    #[test]
    fn mcap_decompression_budget_and_worker_pool_are_service_bounded() {
        assert_eq!(MAX_MCAP_CHUNK_UNCOMPRESSED_BYTES, 64 * 1024 * 1024);
        assert_eq!(MAX_MCAP_TOTAL_UNCOMPRESSED_BYTES, 2 * 1024 * 1024 * 1024);
        assert_eq!(MAX_MCAP_DECODE_WORKERS, 2);

        let pool = MCAP_DECODE_POOL
            .as_ref()
            .expect("bounded MCAP decode pool should build");
        assert_eq!(pool.current_num_threads(), MAX_MCAP_DECODE_WORKERS);
        assert_eq!(
            pool.install(rayon::current_num_threads),
            MAX_MCAP_DECODE_WORKERS,
            "re_mcap must observe at most two workers and select its serial decode path"
        );
    }

    #[test]
    fn mcap_summary_record_count_is_bounded_before_owned_parse() {
        let mut bytes = mcap::MAGIC.to_vec();
        let summary_start = bytes.len() as u64;
        for _ in 0..=MAX_MCAP_SUMMARY_RECORDS {
            bytes.push(mcap::records::op::SCHEMA);
            bytes.extend_from_slice(&0_u64.to_le_bytes());
        }
        bytes.push(mcap::records::op::FOOTER);
        bytes.extend_from_slice(&20_u64.to_le_bytes());
        bytes.extend_from_slice(&summary_start.to_le_bytes());
        bytes.extend_from_slice(&0_u64.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        bytes.extend_from_slice(mcap::MAGIC);

        let error = preflight_mcap_summary(&bytes, Path::new("adversarial.mcap"))
            .expect_err("summary record bomb must fail before mcap::Summary allocation");
        assert_eq!(error.code(), ImportErrorCode::ResourceLimitExceeded);
    }

    #[test]
    fn mcap_summary_chunk_index_count_is_bounded_before_owned_parse() {
        let mut bytes = mcap::MAGIC.to_vec();
        let summary_start = bytes.len() as u64;
        for _ in 0..=MAX_MCAP_CHUNKS {
            bytes.push(mcap::records::op::CHUNK_INDEX);
            bytes.extend_from_slice(&0_u64.to_le_bytes());
        }
        bytes.push(mcap::records::op::FOOTER);
        bytes.extend_from_slice(&20_u64.to_le_bytes());
        bytes.extend_from_slice(&summary_start.to_le_bytes());
        bytes.extend_from_slice(&0_u64.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        bytes.extend_from_slice(mcap::MAGIC);

        let error = preflight_mcap_summary(&bytes, Path::new("chunk-index-bomb.mcap"))
            .expect_err("chunk-index bomb must fail before mcap::Summary allocation");
        assert_eq!(error.code(), ImportErrorCode::ResourceLimitExceeded);
    }

    #[test]
    fn mcap_attachment_and_metadata_records_are_request_bounded() {
        let cursor = std::io::Cursor::new(Vec::new());
        let mut writer = mcap::Writer::new(cursor).expect("create MCAP writer");
        let channel_id = writer
            .add_channel(0, "topic", "raw", &BTreeMap::new())
            .expect("add channel");
        writer
            .write_to_known_channel(
                &mcap::records::MessageHeader {
                    channel_id,
                    sequence: 0,
                    log_time: 1,
                    publish_time: 1,
                },
                &[1],
            )
            .expect("write message");
        writer.flush().expect("flush indexed chunk");
        writer
            .attach(&mcap::Attachment {
                log_time: 1,
                create_time: 1,
                name: "calibration".to_owned(),
                media_type: "application/octet-stream".to_owned(),
                data: Cow::Borrowed(&[1]),
            })
            .expect("write attachment");
        writer
            .write_metadata(&mcap::records::Metadata {
                name: "device".to_owned(),
                metadata: BTreeMap::from([("id".to_owned(), "robot-1".to_owned())]),
            })
            .expect("write metadata");
        let summary = writer.finish().expect("finish MCAP");
        let bytes = writer.into_inner().into_inner();
        validate_mcap_resources(
            &summary,
            &bytes,
            TEST_OUTPUT_LIMIT,
            Path::new("bounded.mcap"),
        )
        .expect("small indexed resources are valid");

        let mut oversized_attachment = summary.clone();
        oversized_attachment.attachment_indexes[0].data_size = MAX_MCAP_ATTACHMENT_DATA_BYTES + 1;
        let error = validate_mcap_resources(
            &oversized_attachment,
            &bytes,
            TEST_OUTPUT_LIMIT,
            Path::new("attachment-bomb.mcap"),
        )
        .expect_err("oversized attachment must fail before decoding");
        assert_eq!(error.code(), ImportErrorCode::ResourceLimitExceeded);

        let mut oversized_metadata = summary;
        oversized_metadata.metadata_indexes[0].length = MAX_MCAP_TOTAL_METADATA_BYTES + 1;
        let error = validate_mcap_resources(
            &oversized_metadata,
            &bytes,
            TEST_OUTPUT_LIMIT,
            Path::new("metadata-bomb.mcap"),
        )
        .expect_err("oversized metadata must fail before decoding");
        assert_eq!(error.code(), ImportErrorCode::ResourceLimitExceeded);

        assert_eq!(MAX_MCAP_ATTACHMENTS, 1_024);
        assert_eq!(MAX_MCAP_TOTAL_ATTACHMENT_BYTES, 128 * 1024 * 1024);
    }
}
