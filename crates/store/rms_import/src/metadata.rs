use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::Path;

use re_chunk::external::arrow::array::{Array as _, BooleanArray};
use re_log_encoding::{Decodable as _, RawRrdManifest, RrdManifest, StreamFooter, StreamHeader};
use re_log_types::{StoreKind, TimeType};
use sha2::{Digest as _, Sha256};

use crate::{
    EntityDescriptor, EntityVisualization, ImportCancellation, ImportError, ImportErrorCode,
    TimelineDescriptor, TimelineKind, check_cancelled,
};

const MAX_RRD_CHUNKS: usize = 65_536;
const MAX_RRD_FOOTER_BYTES: u64 = 64 * 1024 * 1024;
const MAX_RRD_MANIFESTS: usize = 64;
const MAX_RRD_MANIFEST_BYTES: usize = 32 * 1024 * 1024;
const MAX_RRD_MANIFEST_ARROW_BYTES: u64 = 64 * 1024 * 1024;
const MAX_RRD_FOOTER_DECODE_BUDGET_BYTES: u64 = 128 * 1024 * 1024;
const MAX_RRD_SCHEMA_BYTES: usize = 8 * 1024 * 1024;
const MAX_RRD_CHUNK_COMPRESSED_BYTES: u64 = 64 * 1024 * 1024;
const MAX_RRD_CHUNK_UNCOMPRESSED_BYTES: u64 = 64 * 1024 * 1024;
const MAX_RRD_TOTAL_COMPRESSED_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_RRD_TOTAL_UNCOMPRESSED_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_RRD_COMPRESSION_RATIO: u64 = 1000;
const RRD_DECODE_BATCH_BYTES: u64 = 64 * 1024 * 1024;
const RRD_DECODE_BATCH_CHUNKS: usize = 32;

pub(crate) struct VerifiedRrd {
    pub content_sha256: String,
    pub size_bytes: u64,
    pub rrd_version: String,
    pub timelines: Vec<TimelineDescriptor>,
    pub default_timeline: String,
    pub duration_seconds: f64,
    pub entity_paths: Vec<String>,
    pub entity_descriptors: Vec<EntityDescriptor>,
}

fn archetype_visualization(short_name: &str) -> Option<EntityVisualization> {
    match short_name {
        "EncodedImage"
        | "EncodedDepthImage"
        | "Image"
        | "DepthImage"
        | "SegmentationImage"
        | "VideoStream"
        | "VideoFrameReference" => Some(EntityVisualization::Camera),
        "GeoPoints" | "GeoLineStrings" => Some(EntityVisualization::Map),
        "Points2D" | "Boxes2D" | "Arrows2D" | "LineStrips2D" | "Ellipses2D" => {
            Some(EntityVisualization::Spatial2d)
        }
        // Grid maps are planar, but Rerun's visualizer treats them as textured rectangles in a
        // 3D space (and explicitly prefers `SpatialView3D`). Sending them to a 2D view produces an
        // empty view with a transform warning instead of the recorded occupancy/cost map.
        "GridMap" | "VoxelGridMap" | "Points3D" | "Boxes3D" | "Arrows3D" | "LineStrips3D"
        | "Mesh3D" | "Asset3D" | "Capsules3D" | "Cylinders3D" | "Ellipsoids3D"
        | "GaussianSplats3D" | "Pinhole" => Some(EntityVisualization::Spatial3d),
        "Transform3D" | "TransformAxes3D" => Some(EntityVisualization::Transform3d),
        "Scalars" | "SeriesLines" | "SeriesPoints" => Some(EntityVisualization::TimeSeries),
        "TextLog" => Some(EntityVisualization::Log),
        // A coordinate-frame component only labels data with its ROS frame. It has no geometry of
        // its own, so it (like any other metadata-only archetype) must not turn entities such as
        // JointState into empty 3D views. A drawable archetype on the same entity will still
        // determine the renderer.
        _ => None,
    }
}

fn visualization_priority(visualization: EntityVisualization) -> u8 {
    match visualization {
        EntityVisualization::Camera => 8,
        EntityVisualization::Map => 7,
        EntityVisualization::Spatial2d => 6,
        EntityVisualization::Spatial3d => 5,
        // Prefer drawable geometry when an entity contains both geometry and a transform.
        EntityVisualization::Transform3d => 4,
        EntityVisualization::TimeSeries => 3,
        EntityVisualization::State => 2,
        EntityVisualization::Log => 1,
        EntityVisualization::Raw => 0,
    }
}

fn recording_entity_descriptors(
    manifest: &RrdManifest,
    entity_paths: &BTreeSet<String>,
) -> Vec<EntityDescriptor> {
    let mut visualizations = entity_paths
        .iter()
        .map(|path| (path.clone(), EntityVisualization::Raw))
        .collect::<BTreeMap<_, _>>();

    for component in manifest.recording_schema().columns.component_columns() {
        let Some(archetype) = component.archetype else {
            continue;
        };
        let Some(candidate) = archetype_visualization(archetype.short_name()) else {
            continue;
        };
        let path = component.entity_path.to_string();
        let current = visualizations
            .entry(path)
            .or_insert(EntityVisualization::Raw);
        if visualization_priority(candidate) > visualization_priority(*current) {
            *current = candidate;
        }
    }

    visualizations
        .into_iter()
        .map(|(path, visualization)| EntityDescriptor {
            path,
            visualization,
        })
        .collect()
}

pub(crate) fn sha256_file(
    path: &Path,
    cancellation: Option<&ImportCancellation>,
) -> Result<String, ImportError> {
    let mut file = std::fs::File::open(path).map_err(|err| {
        ImportError::new(
            ImportErrorCode::Io,
            "The source file could not be read.",
            format!(
                "Failed to open source for hashing: {err}\nFile path: {}",
                path.display()
            ),
        )
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        check_cancelled(cancellation)?;
        let count = file.read(&mut buffer).map_err(|err| {
            ImportError::new(
                ImportErrorCode::Io,
                "The source file could not be read.",
                format!(
                    "Failed while hashing source: {err}\nFile path: {}",
                    path.display()
                ),
            )
        })?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[expect(
    clippy::iter_over_hash_type,
    reason = "manifest traversals feed order-independent sets, minima, maxima, and sorted spans"
)]
pub(crate) fn verify_rrd(
    path: &Path,
    default_timeline_hint: Option<&str>,
    fps_hints: &BTreeMap<String, f64>,
    cancellation: Option<&ImportCancellation>,
) -> Result<VerifiedRrd, ImportError> {
    check_cancelled(cancellation)?;
    let mut file = std::fs::File::open(path).map_err(|err| {
        ImportError::new(
            ImportErrorCode::OutputValidationFailed,
            "The converted recording could not be verified.",
            format!(
                "Failed to open RRD for verification: {err}\nFile path: {}",
                path.display()
            ),
        )
    })?;
    let size_bytes = file
        .metadata()
        .map_err(|err| {
            ImportError::new(
                ImportErrorCode::OutputValidationFailed,
                "The converted recording could not be verified.",
                format!(
                    "Failed to inspect RRD: {err}\nFile path: {}",
                    path.display()
                ),
            )
        })?
        .len();
    if size_bytes <= StreamHeader::ENCODED_SIZE_BYTES as u64 {
        return Err(ImportError::new(
            ImportErrorCode::OutputValidationFailed,
            "The converted recording contains no Replay data.",
            format!(
                "RRD is only {size_bytes} bytes\nFile path: {}",
                path.display()
            ),
        ));
    }

    let mut header = [0_u8; StreamHeader::ENCODED_SIZE_BYTES];
    file.read_exact(&mut header).map_err(|err| {
        ImportError::new(
            ImportErrorCode::OutputValidationFailed,
            "The converted recording has an invalid RRD header.",
            format!(
                "Failed to read RRD header: {err}\nFile path: {}",
                path.display()
            ),
        )
    })?;
    let stream_header = StreamHeader::from_rrd_bytes(&header).map_err(|err| {
        ImportError::new(
            ImportErrorCode::OutputValidationFailed,
            "The converted recording has an invalid RRD header.",
            format!(
                "Failed to decode RRD header: {err}\nFile path: {}",
                path.display()
            ),
        )
    })?;
    let (version, _) = stream_header.to_version_and_options().map_err(|err| {
        ImportError::new(
            ImportErrorCode::OutputValidationFailed,
            "The converted recording uses an incompatible RRD version.",
            format!(
                "Failed to validate RRD version: {err}\nFile path: {}",
                path.display()
            ),
        )
    })?;

    file.seek(SeekFrom::Start(0)).map_err(|err| {
        ImportError::new(
            ImportErrorCode::OutputValidationFailed,
            "The converted recording could not be verified.",
            format!("Failed to rewind RRD: {err}\nFile path: {}", path.display()),
        )
    })?;
    let footer_payload_span = validate_footer_pointer(&mut file, size_bytes, path)?;
    preflight_rrd_footer_payload(&mut file, footer_payload_span, cancellation, path)?;
    let footer = futures::executor::block_on(re_log_encoding::read_rrd_footer(&file))
        .map_err(|err| {
            ImportError::new(
                ImportErrorCode::OutputValidationFailed,
                "The converted recording footer is corrupt.",
                format!(
                    "RRD footer verification failed: {err}\nFile path: {}",
                    path.display()
                ),
            )
        })?
        .ok_or_else(|| {
            ImportError::new(
                ImportErrorCode::OutputValidationFailed,
                "The converted recording has no RRD footer.",
                format!(
                    "RRD has no random-access footer\nFile path: {}",
                    path.display()
                ),
            )
        })?;

    validate_raw_manifest_row_count(
        footer
            .manifests
            .values()
            .map(|manifest| manifest.data.num_rows()),
        path,
    )?;

    let mut manifests = Vec::with_capacity(footer.manifests.len());
    let mut expected_chunks_by_manifest = Vec::with_capacity(footer.manifests.len());
    for raw_manifest in footer.manifests.values() {
        check_cancelled(cancellation)?;
        let manifest = RrdManifest::try_new(raw_manifest).map_err(|err| {
            ImportError::new(
                ImportErrorCode::OutputValidationFailed,
                "The converted recording manifest is corrupt.",
                format!(
                    "RRD manifest verification failed: {err}\nFile path: {}",
                    path.display()
                ),
            )
        })?;
        expected_chunks_by_manifest.push(expected_chunk_metadata(raw_manifest, &manifest, path)?);
        manifests.push(manifest);
    }
    let recording_manifest_count = manifests
        .iter()
        .filter(|manifest| manifest.store_id().kind() == StoreKind::Recording)
        .count();
    if recording_manifest_count != 1 {
        return Err(ImportError::new(
            ImportErrorCode::OutputValidationFailed,
            "An imported RRD must contain exactly one recording store.",
            format!(
                "RRD footer has {recording_manifest_count} recording manifests; expected exactly one\nFile path: {}",
                path.display()
            ),
        ));
    }

    validate_manifest_resources(
        &file,
        size_bytes,
        &manifests,
        &expected_chunks_by_manifest,
        cancellation,
        path,
    )?;

    let recording_manifest = manifests
        .iter()
        .find(|manifest| manifest.store_id().kind() == StoreKind::Recording)
        .ok_or_else(|| rrd_validation_error("Validated recording manifest is unavailable", path))?;
    let mut ranges = BTreeMap::<(String, TimelineKind), (i64, i64)>::new();
    let mut entity_paths = BTreeSet::new();
    for (entity_path, timelines) in recording_manifest.temporal_map() {
        entity_paths.insert(entity_path.to_string());
        for (timeline, components) in timelines {
            let kind = match timeline.typ() {
                TimeType::Sequence => TimelineKind::Sequence,
                TimeType::TimestampNs => TimelineKind::Timestamp,
                TimeType::DurationNs => TimelineKind::Duration,
            };
            let entry = ranges
                .entry((timeline.name().to_string(), kind))
                .or_insert((i64::MAX, i64::MIN));
            for chunks in components.values() {
                for chunk in chunks.values() {
                    entry.0 = entry.0.min(chunk.time_range.min().as_i64());
                    entry.1 = entry.1.max(chunk.time_range.max().as_i64());
                }
            }
        }
    }
    entity_paths.extend(
        recording_manifest
            .static_map()
            .keys()
            .map(ToString::to_string),
    );
    if ranges.is_empty() {
        return Err(ImportError::new(
            ImportErrorCode::OutputValidationFailed,
            "The recording contains no temporal data to Replay.",
            format!(
                "RRD recording manifests contain no temporal chunks\nFile path: {}",
                path.display()
            ),
        ));
    }

    let timelines = ranges
        .into_iter()
        .map(|((name, kind), (start, end))| {
            let fps = (kind == TimelineKind::Sequence)
                .then(|| fps_hints.get(&name).copied())
                .flatten();
            let duration_seconds = match kind {
                TimelineKind::Timestamp | TimelineKind::Duration => {
                    Some(start.abs_diff(end) as f64 / 1_000_000_000.0)
                }
                TimelineKind::Sequence => fps.map(|fps| start.abs_diff(end) as f64 / fps),
            };
            TimelineDescriptor {
                name,
                kind,
                start: start.to_string(),
                end: end.to_string(),
                duration_seconds,
                fps,
            }
        })
        .collect::<Vec<_>>();
    let default_timeline = default_timeline_hint
        .filter(|hint| timelines.iter().any(|timeline| timeline.name == **hint))
        .map(ToOwned::to_owned)
        .or_else(|| {
            timelines
                .iter()
                .find(|timeline| timeline.name == "message_log_time")
                .map(|timeline| timeline.name.clone())
        })
        .or_else(|| {
            timelines
                .iter()
                .find(|timeline| timeline.kind != TimelineKind::Sequence)
                .map(|timeline| timeline.name.clone())
        })
        .unwrap_or_else(|| timelines[0].name.clone());
    let duration_seconds = timelines
        .iter()
        .filter_map(|timeline| timeline.duration_seconds)
        .fold(0.0_f64, f64::max);
    let content_sha256 = sha256_file(path, cancellation)?;

    let entity_descriptors = recording_entity_descriptors(recording_manifest, &entity_paths);
    Ok(VerifiedRrd {
        content_sha256,
        size_bytes,
        rrd_version: version.to_string(),
        timelines,
        default_timeline,
        duration_seconds,
        entity_paths: entity_paths.into_iter().collect(),
        entity_descriptors,
    })
}

fn validate_raw_manifest_row_count(
    row_counts: impl IntoIterator<Item = usize>,
    path: &Path,
) -> Result<(), ImportError> {
    let mut total_rows = 0_usize;
    for rows in row_counts {
        total_rows = total_rows
            .checked_add(rows)
            .ok_or_else(|| rrd_resource_error("RRD manifest row count overflow", path))?;
        if total_rows > MAX_RRD_CHUNKS {
            return Err(rrd_resource_error(
                format!("RRD has more than {MAX_RRD_CHUNKS} manifest rows"),
                path,
            ));
        }
    }
    Ok(())
}

fn validate_manifest_resources(
    file: &std::fs::File,
    file_size: u64,
    manifests: &[RrdManifest],
    expected_chunks_by_manifest: &[std::collections::HashMap<
        re_chunk::ChunkId,
        ExpectedChunkMetadata,
    >],
    cancellation: Option<&ImportCancellation>,
    path: &Path,
) -> Result<(), ImportError> {
    let mut spans = Vec::new();
    let mut total_chunks = 0_usize;
    let mut total_compressed = 0_u64;
    let mut total_uncompressed = 0_u64;
    for manifest in manifests {
        check_cancelled(cancellation)?;
        total_chunks = total_chunks
            .checked_add(manifest.num_chunks())
            .ok_or_else(|| rrd_resource_error("RRD chunk count overflow", path))?;
        if total_chunks > MAX_RRD_CHUNKS {
            return Err(rrd_resource_error(
                format!("RRD has more than {MAX_RRD_CHUNKS} chunks"),
                path,
            ));
        }
        let offsets = manifest.col_chunk_byte_offset();
        let sizes = manifest.col_chunk_byte_size();
        let uncompressed_sizes = manifest.col_chunk_byte_size_uncompressed();
        for (index, ((offset, size), uncompressed)) in offsets
            .iter()
            .zip(sizes)
            .zip(uncompressed_sizes)
            .enumerate()
        {
            check_cancelled(cancellation)?;
            if *size == 0 || *size > MAX_RRD_CHUNK_COMPRESSED_BYTES {
                return Err(rrd_resource_error(
                    format!(
                        "RRD chunk {index} has invalid compressed size {size}; maximum is {MAX_RRD_CHUNK_COMPRESSED_BYTES}"
                    ),
                    path,
                ));
            }
            if *uncompressed == 0 || *uncompressed > MAX_RRD_CHUNK_UNCOMPRESSED_BYTES {
                return Err(rrd_resource_error(
                    format!(
                        "RRD chunk {index} has invalid uncompressed size {uncompressed}; maximum is {MAX_RRD_CHUNK_UNCOMPRESSED_BYTES}"
                    ),
                    path,
                ));
            }
            if *uncompressed > size.saturating_mul(MAX_RRD_COMPRESSION_RATIO) {
                return Err(rrd_resource_error(
                    format!(
                        "RRD chunk {index} exceeds compression ratio {MAX_RRD_COMPRESSION_RATIO}:1"
                    ),
                    path,
                ));
            }
            total_compressed = total_compressed
                .checked_add(*size)
                .ok_or_else(|| rrd_resource_error("RRD total compressed size overflow", path))?;
            if total_compressed > MAX_RRD_TOTAL_COMPRESSED_BYTES {
                return Err(rrd_resource_error(
                    format!(
                        "RRD total referenced compressed size exceeds {MAX_RRD_TOTAL_COMPRESSED_BYTES} bytes"
                    ),
                    path,
                ));
            }
            total_uncompressed = total_uncompressed
                .checked_add(*uncompressed)
                .ok_or_else(|| rrd_resource_error("RRD total uncompressed size overflow", path))?;
            if total_uncompressed > MAX_RRD_TOTAL_UNCOMPRESSED_BYTES {
                return Err(rrd_resource_error(
                    format!(
                        "RRD total uncompressed size exceeds {MAX_RRD_TOTAL_UNCOMPRESSED_BYTES} bytes"
                    ),
                    path,
                ));
            }
            let end = offset
                .checked_add(*size)
                .ok_or_else(|| rrd_validation_error("RRD chunk byte span overflow", path))?;
            if *offset < StreamHeader::ENCODED_SIZE_BYTES as u64 || end > file_size {
                return Err(rrd_validation_error(
                    format!("RRD chunk span {offset}..{end} is outside file bounds 0..{file_size}"),
                    path,
                ));
            }
            spans.push((*offset, end));
        }
    }
    validate_rrd_chunk_span_set(&mut spans, path)?;

    let mut payload_reader = file
        .try_clone()
        .map_err(|err| rrd_validation_error(format!("Failed to clone RRD reader: {err}"), path))?;
    for manifest in manifests {
        let offsets = manifest.col_chunk_byte_offset();
        let sizes = manifest.col_chunk_byte_size();
        let uncompressed_sizes = manifest.col_chunk_byte_size_uncompressed();
        let chunk_ids = manifest.col_chunk_ids();
        for (index, ((offset, size), uncompressed)) in offsets
            .iter()
            .zip(sizes)
            .zip(uncompressed_sizes)
            .enumerate()
        {
            check_cancelled(cancellation)?;
            validate_arrow_transport(
                &mut payload_reader,
                *offset,
                *size,
                *uncompressed,
                manifest.store_id(),
                chunk_ids[index],
                path,
            )?;
        }
    }

    for (manifest, expected_chunks) in manifests.iter().zip(expected_chunks_by_manifest) {
        check_cancelled(cancellation)?;
        let chunk_ids = manifest.col_chunk_ids();
        let sizes = manifest.col_chunk_byte_size_uncompressed();
        let entity_paths = manifest.col_chunk_entity_path().collect::<Vec<_>>();
        let is_static = manifest.col_chunk_is_static().collect::<Vec<_>>();
        let row_by_id = chunk_ids
            .iter()
            .enumerate()
            .map(|(row, chunk_id)| (*chunk_id, row))
            .collect::<std::collections::HashMap<_, _>>();
        let mut start = 0_usize;
        while start < chunk_ids.len() {
            check_cancelled(cancellation)?;
            let mut end = start;
            let mut batch_bytes = 0_u64;
            while end < chunk_ids.len() && end - start < RRD_DECODE_BATCH_CHUNKS {
                let next = batch_bytes.saturating_add(sizes[end]);
                if end > start && next > RRD_DECODE_BATCH_BYTES {
                    break;
                }
                batch_bytes = next;
                end += 1;
            }
            let decoded = futures::executor::block_on(re_log_encoding::rrd::read_chunks(
                file,
                manifest,
                &chunk_ids[start..end],
            ))
            .map_err(|err| {
                rrd_validation_error(format!("RRD chunk payload decoding failed: {err}"), path)
            })?;
            for chunk in decoded {
                let row = row_by_id.get(&chunk.id()).copied().ok_or_else(|| {
                    rrd_validation_error(
                        format!(
                            "Decoded RRD chunk {} is absent from its manifest",
                            chunk.id()
                        ),
                        path,
                    )
                })?;
                if chunk.entity_path() != &entity_paths[row] || chunk.is_static() != is_static[row]
                {
                    return Err(rrd_validation_error(
                        format!(
                            "Decoded RRD chunk {} identity metadata disagrees with manifest row {row}",
                            chunk.id()
                        ),
                        path,
                    ));
                }
                validate_decoded_chunk_metadata(&chunk, expected_chunks, path)?;
            }
            start = end;
        }
    }
    Ok(())
}

fn validate_rrd_chunk_span_set(spans: &mut [(u64, u64)], path: &Path) -> Result<(), ImportError> {
    spans.sort_unstable();
    for adjacent in spans.windows(2) {
        if adjacent[1].0 < adjacent[0].1 {
            return Err(rrd_validation_error(
                format!(
                    "RRD chunk spans overlap: {:?} and {:?}",
                    adjacent[0], adjacent[1]
                ),
                path,
            ));
        }
    }
    Ok(())
}

fn validate_footer_pointer(
    file: &mut std::fs::File,
    file_size: u64,
    path: &Path,
) -> Result<(u64, u64), ImportError> {
    let footer_size = StreamFooter::ENCODED_SIZE_BYTES as u64;
    if file_size < footer_size {
        return Err(rrd_validation_error(
            "RRD is too small to contain a footer",
            path,
        ));
    }
    file.seek(SeekFrom::Start(file_size - footer_size))
        .map_err(|err| {
            rrd_validation_error(format!("Failed to seek to RRD footer: {err}"), path)
        })?;
    let mut footer_bytes = vec![0_u8; StreamFooter::ENCODED_SIZE_BYTES];
    file.read_exact(&mut footer_bytes)
        .map_err(|err| rrd_validation_error(format!("Failed to read RRD footer: {err}"), path))?;
    let footer = StreamFooter::from_rrd_bytes(&footer_bytes).map_err(|err| {
        rrd_validation_error(format!("Failed to decode RRD stream footer: {err}"), path)
    })?;
    if footer.entries.len() != 1 {
        return Err(rrd_validation_error(
            format!(
                "RRD stream footer has {} entries; expected exactly one",
                footer.entries.len()
            ),
            path,
        ));
    }
    let span = footer.entries[0].rrd_footer_byte_span_from_start_excluding_header;
    if span.len == 0 || span.len > MAX_RRD_FOOTER_BYTES {
        return Err(rrd_resource_error(
            format!(
                "RRD footer payload is {} bytes; maximum is {MAX_RRD_FOOTER_BYTES}",
                span.len
            ),
            path,
        ));
    }
    let end = span
        .start
        .checked_add(span.len)
        .ok_or_else(|| rrd_validation_error("RRD footer span overflow", path))?;
    if span.start < StreamHeader::ENCODED_SIZE_BYTES as u64 || end > file_size - footer_size {
        return Err(rrd_validation_error(
            format!(
                "RRD footer payload span {}..{end} is outside the payload region",
                span.start
            ),
            path,
        ));
    }
    Ok((span.start, span.len))
}

fn preflight_rrd_footer_payload(
    file: &mut std::fs::File,
    (start, len): (u64, u64),
    cancellation: Option<&ImportCancellation>,
    path: &Path,
) -> Result<(), ImportError> {
    let len = usize::try_from(len).map_err(|err| {
        rrd_resource_error(format!("RRD footer size does not fit memory: {err}"), path)
    })?;
    file.seek(SeekFrom::Start(start)).map_err(|err| {
        rrd_validation_error(format!("Failed to seek to RRD footer payload: {err}"), path)
    })?;
    let mut payload = vec![0_u8; len];
    for chunk in payload.chunks_mut(1024 * 1024) {
        check_cancelled(cancellation)?;
        file.read_exact(chunk).map_err(|err| {
            rrd_validation_error(format!("Failed to read RRD footer payload: {err}"), path)
        })?;
    }
    preflight_rrd_footer_wire(&payload, path)
}

fn preflight_rrd_footer_wire(payload: &[u8], path: &Path) -> Result<(), ImportError> {
    let mut cursor = 0_usize;
    let mut manifests = 0_usize;
    let mut decode_budget = 0_u64;
    while cursor < payload.len() {
        let (field, wire_type) = read_proto_key(payload, &mut cursor, path)?;
        if field != 1 || wire_type != 2 {
            return Err(rrd_validation_error(
                format!(
                    "RRD footer contains unexpected protobuf field {field} with wire type {wire_type}"
                ),
                path,
            ));
        }
        let manifest = read_proto_bytes(payload, &mut cursor, path)?;
        manifests += 1;
        if manifests > MAX_RRD_MANIFESTS {
            return Err(rrd_resource_error(
                format!("RRD footer has more than {MAX_RRD_MANIFESTS} manifests"),
                path,
            ));
        }
        if manifest.len() > MAX_RRD_MANIFEST_BYTES {
            return Err(rrd_resource_error(
                format!(
                    "RRD manifest is {} bytes; maximum is {MAX_RRD_MANIFEST_BYTES}",
                    manifest.len()
                ),
                path,
            ));
        }
        let (encoded_bytes, decoded_bytes) = preflight_rrd_manifest_wire(manifest, path)?;
        decode_budget = decode_budget
            .checked_add(encoded_bytes)
            .and_then(|total| total.checked_add(decoded_bytes))
            .ok_or_else(|| rrd_resource_error("RRD footer decode budget overflow", path))?;
        if decode_budget > MAX_RRD_FOOTER_DECODE_BUDGET_BYTES {
            return Err(rrd_resource_error(
                format!(
                    "RRD footer Arrow payloads require more than {MAX_RRD_FOOTER_DECODE_BUDGET_BYTES} bytes while decoding"
                ),
                path,
            ));
        }
    }
    if manifests == 0 {
        return Err(rrd_validation_error(
            "RRD footer contains no manifests",
            path,
        ));
    }
    Ok(())
}

fn preflight_rrd_manifest_wire(manifest: &[u8], path: &Path) -> Result<(u64, u64), ImportError> {
    let mut cursor = 0_usize;
    let mut seen = [false; 4];
    let mut dataframe_budget = None;
    while cursor < manifest.len() {
        let (field, wire_type) = read_proto_key(manifest, &mut cursor, path)?;
        let index = usize::try_from(field.saturating_sub(1)).unwrap_or(usize::MAX);
        if !(1..=4).contains(&field) || seen[index] {
            return Err(rrd_validation_error(
                format!("RRD manifest contains duplicate or unknown protobuf field {field}"),
                path,
            ));
        }
        seen[index] = true;
        match field {
            1 if wire_type == 2 => {
                let store_id = read_proto_bytes(manifest, &mut cursor, path)?;
                if store_id.len() > 16 * 1024 {
                    return Err(rrd_resource_error(
                        "RRD manifest StoreId exceeds 16 KiB",
                        path,
                    ));
                }
            }
            2 if wire_type == 2 => {
                preflight_rrd_schema_wire(read_proto_bytes(manifest, &mut cursor, path)?, path)?;
            }
            3 if wire_type == 2 => {
                let sha256 = read_proto_bytes(manifest, &mut cursor, path)?;
                if sha256.len() != 32 {
                    return Err(rrd_validation_error(
                        format!(
                            "RRD manifest schema SHA256 is {} bytes; expected 32",
                            sha256.len()
                        ),
                        path,
                    ));
                }
            }
            4 if wire_type == 2 => {
                dataframe_budget = Some(preflight_rrd_dataframe_wire(
                    read_proto_bytes(manifest, &mut cursor, path)?,
                    path,
                )?);
            }
            _ => {
                return Err(rrd_validation_error(
                    format!("RRD manifest field {field} has unsupported wire type {wire_type}"),
                    path,
                ));
            }
        }
    }
    if seen.iter().any(|seen| !seen) {
        return Err(rrd_validation_error(
            "RRD manifest is missing one or more required fields",
            path,
        ));
    }
    dataframe_budget
        .ok_or_else(|| rrd_validation_error("RRD manifest is missing its DataframePart", path))
}

fn preflight_rrd_schema_wire(schema: &[u8], path: &Path) -> Result<(), ImportError> {
    let mut cursor = 0_usize;
    let (field, wire_type) = read_proto_key(schema, &mut cursor, path)?;
    if field != 1 || wire_type != 2 {
        return Err(rrd_validation_error(
            "RRD manifest Sorbet schema has an invalid protobuf shape",
            path,
        ));
    }
    let arrow_schema = read_proto_bytes(schema, &mut cursor, path)?;
    if arrow_schema.is_empty() || arrow_schema.len() > MAX_RRD_SCHEMA_BYTES {
        return Err(rrd_resource_error(
            format!(
                "RRD Sorbet schema is {} bytes; maximum is {MAX_RRD_SCHEMA_BYTES}",
                arrow_schema.len()
            ),
            path,
        ));
    }
    if cursor != schema.len() {
        return Err(rrd_validation_error(
            "RRD manifest Sorbet schema contains duplicate or unknown fields",
            path,
        ));
    }
    Ok(())
}

fn preflight_rrd_dataframe_wire(data: &[u8], path: &Path) -> Result<(u64, u64), ImportError> {
    let mut cursor = 0_usize;
    let mut encoder_version = None;
    let mut payload_size = None;
    let mut compression = None;
    let mut uncompressed_size = None;
    while cursor < data.len() {
        let (field, wire_type) = read_proto_key(data, &mut cursor, path)?;
        match field {
            1 if wire_type == 0 && encoder_version.is_none() => {
                encoder_version = Some(read_proto_varint(data, &mut cursor, path)?);
            }
            2 if wire_type == 2 && payload_size.is_none() => {
                let payload = read_proto_bytes(data, &mut cursor, path)?;
                payload_size = Some(u64::try_from(payload.len()).map_err(|err| {
                    rrd_resource_error(format!("RRD manifest payload size overflow: {err}"), path)
                })?);
            }
            3 if wire_type == 0 && compression.is_none() => {
                compression = Some(read_proto_varint(data, &mut cursor, path)?);
            }
            4 if wire_type == 0 && uncompressed_size.is_none() => {
                uncompressed_size = Some(read_proto_varint(data, &mut cursor, path)?);
            }
            _ => {
                return Err(rrd_validation_error(
                    format!(
                        "RRD manifest DataframePart contains duplicate/unknown field {field} or wire type {wire_type}"
                    ),
                    path,
                ));
            }
        }
    }
    let payload_size = payload_size.ok_or_else(|| {
        rrd_validation_error("RRD manifest DataframePart has no Arrow payload", path)
    })?;
    if payload_size == 0 || payload_size > MAX_RRD_MANIFEST_ARROW_BYTES {
        return Err(rrd_resource_error(
            format!(
                "RRD manifest Arrow payload is {payload_size} bytes; maximum is {MAX_RRD_MANIFEST_ARROW_BYTES}"
            ),
            path,
        ));
    }
    let compression = compression.unwrap_or(0);
    let uncompressed_size = uncompressed_size.unwrap_or(0);
    if uncompressed_size > MAX_RRD_MANIFEST_ARROW_BYTES {
        return Err(rrd_resource_error(
            format!(
                "RRD manifest declares {uncompressed_size} uncompressed Arrow bytes; maximum is {MAX_RRD_MANIFEST_ARROW_BYTES}"
            ),
            path,
        ));
    }
    match compression {
        0 | 1 => {}
        2 => {
            if uncompressed_size == 0
                || uncompressed_size > payload_size.saturating_mul(MAX_RRD_COMPRESSION_RATIO)
            {
                return Err(rrd_resource_error(
                    format!(
                        "RRD manifest Arrow payload exceeds compression ratio {MAX_RRD_COMPRESSION_RATIO}:1"
                    ),
                    path,
                ));
            }
        }
        other => {
            return Err(rrd_validation_error(
                format!("RRD manifest uses unknown compression value {other}"),
                path,
            ));
        }
    }
    let decoded_size = if compression == 2 {
        uncompressed_size
    } else {
        payload_size
    };
    let _ = encoder_version;
    Ok((payload_size, decoded_size))
}

fn read_proto_key(bytes: &[u8], cursor: &mut usize, path: &Path) -> Result<(u64, u8), ImportError> {
    let key = read_proto_varint(bytes, cursor, path)?;
    let field = key >> 3;
    let wire_type = (key & 0x07) as u8;
    if field == 0 {
        return Err(rrd_validation_error("Protobuf field number is zero", path));
    }
    Ok((field, wire_type))
}

fn read_proto_bytes<'a>(
    bytes: &'a [u8],
    cursor: &mut usize,
    path: &Path,
) -> Result<&'a [u8], ImportError> {
    let len = usize::try_from(read_proto_varint(bytes, cursor, path)?).map_err(|err| {
        rrd_resource_error(
            format!("Protobuf byte length does not fit memory: {err}"),
            path,
        )
    })?;
    let end = cursor
        .checked_add(len)
        .ok_or_else(|| rrd_validation_error("Protobuf byte span overflow", path))?;
    let value = bytes
        .get(*cursor..end)
        .ok_or_else(|| rrd_validation_error("Protobuf byte field is truncated", path))?;
    *cursor = end;
    Ok(value)
}

fn read_proto_varint(bytes: &[u8], cursor: &mut usize, path: &Path) -> Result<u64, ImportError> {
    let mut value = 0_u64;
    for shift in (0..70).step_by(7) {
        let byte = *bytes
            .get(*cursor)
            .ok_or_else(|| rrd_validation_error("Protobuf varint is truncated", path))?;
        *cursor += 1;
        if shift == 63 && byte > 1 {
            return Err(rrd_validation_error("Protobuf varint overflows u64", path));
        }
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(rrd_validation_error("Protobuf varint is too long", path))
}

fn validate_arrow_transport(
    file: &mut std::fs::File,
    offset: u64,
    compressed_size: u64,
    manifest_uncompressed_size: u64,
    manifest_store_id: &re_log_types::StoreId,
    manifest_chunk_id: re_chunk::ChunkId,
    path: &Path,
) -> Result<(), ImportError> {
    let compressed_size = usize::try_from(compressed_size).map_err(|err| {
        rrd_resource_error(format!("RRD chunk size does not fit memory: {err}"), path)
    })?;
    file.seek(SeekFrom::Start(offset)).map_err(|err| {
        rrd_validation_error(format!("Failed to seek to RRD chunk payload: {err}"), path)
    })?;
    let mut payload = vec![0_u8; compressed_size];
    file.read_exact(&mut payload).map_err(|err| {
        rrd_validation_error(format!("Failed to read RRD chunk payload: {err}"), path)
    })?;
    let arrow_msg =
        re_protos::log_msg::v1alpha1::ArrowMsg::from_rrd_bytes(&payload).map_err(|err| {
            rrd_validation_error(
                format!("Failed to parse RRD ArrowMsg transport: {err}"),
                path,
            )
        })?;
    if arrow_msg.uncompressed_size != manifest_uncompressed_size {
        return Err(rrd_validation_error(
            format!(
                "RRD ArrowMsg uncompressed_size {} disagrees with manifest {}",
                arrow_msg.uncompressed_size, manifest_uncompressed_size
            ),
            path,
        ));
    }
    let expected_store_id: re_protos::common::v1alpha1::StoreId = manifest_store_id.clone().into();
    let expected_chunk_id: re_protos::common::v1alpha1::Tuid = (*manifest_chunk_id).into();
    if arrow_msg.store_id.as_ref() != Some(&expected_store_id)
        || arrow_msg.chunk_id.as_ref() != Some(&expected_chunk_id)
    {
        return Err(rrd_validation_error(
            format!(
                "RRD ArrowMsg store/chunk identity disagrees with manifest store={manifest_store_id} chunk={manifest_chunk_id}"
            ),
            path,
        ));
    }
    usize::try_from(arrow_msg.uncompressed_size).map_err(|err| {
        rrd_resource_error(
            format!("RRD ArrowMsg uncompressed_size does not fit memory: {err}"),
            path,
        )
    })?;
    let encoded_payload_size = u64::try_from(arrow_msg.payload.len()).map_err(|err| {
        rrd_resource_error(format!("RRD ArrowMsg payload size overflow: {err}"), path)
    })?;
    if encoded_payload_size == 0
        || arrow_msg.uncompressed_size
            > encoded_payload_size.saturating_mul(MAX_RRD_COMPRESSION_RATIO)
    {
        return Err(rrd_resource_error(
            format!("RRD ArrowMsg payload exceeds compression ratio {MAX_RRD_COMPRESSION_RATIO}:1"),
            path,
        ));
    }
    Ok(())
}

#[derive(Default)]
struct ExpectedChunkMetadata {
    num_rows: u64,
    components: BTreeSet<String>,
    timelines: BTreeMap<(String, TimelineKind), (i64, i64)>,
}

#[expect(
    clippy::iter_over_hash_type,
    reason = "manifest entries are aggregated into order-independent validation sets"
)]
fn expected_chunk_metadata(
    raw_manifest: &RawRrdManifest,
    manifest: &RrdManifest,
    path: &Path,
) -> Result<std::collections::HashMap<re_chunk::ChunkId, ExpectedChunkMetadata>, ImportError> {
    let chunk_ids = manifest.col_chunk_ids();
    let chunk_num_rows = manifest.col_chunk_num_rows();
    let chunk_is_static = manifest.col_chunk_is_static().collect::<Vec<_>>();
    let mut expected = std::collections::HashMap::with_capacity(chunk_ids.len());
    for (chunk_id, num_rows) in chunk_ids.iter().zip(chunk_num_rows) {
        if expected
            .insert(
                *chunk_id,
                ExpectedChunkMetadata {
                    num_rows: *num_rows,
                    ..Default::default()
                },
            )
            .is_some()
        {
            return Err(rrd_validation_error(
                format!("RRD manifest contains duplicate chunk ID {chunk_id}"),
                path,
            ));
        }
    }

    let schema = raw_manifest.data.schema_ref();
    for (field, column) in schema.fields().iter().zip(raw_manifest.data.columns()) {
        if !field.name().ends_with(":has_static_data") {
            continue;
        }
        let component = field.metadata().get("rerun:component").ok_or_else(|| {
            rrd_validation_error(
                format!(
                    "RRD static manifest column {} has no component metadata",
                    field.name()
                ),
                path,
            )
        })?;
        let has_static_data = column
            .as_any()
            .downcast_ref::<BooleanArray>()
            .ok_or_else(|| {
                rrd_validation_error(
                    format!(
                        "RRD static manifest column {} has type {}, expected Boolean",
                        field.name(),
                        column.data_type()
                    ),
                    path,
                )
            })?;
        for (row, has_data) in has_static_data.values().iter().enumerate() {
            if !has_data {
                continue;
            }
            if !chunk_is_static[row] {
                return Err(rrd_validation_error(
                    format!("RRD temporal manifest row {row} claims static component {component}"),
                    path,
                ));
            }
            expected
                .get_mut(&chunk_ids[row])
                .ok_or_else(|| {
                    rrd_validation_error(
                        format!("RRD manifest row {row} has no chunk metadata"),
                        path,
                    )
                })?
                .components
                .insert(component.clone());
        }
    }

    for (start_index, start_field) in schema.fields().iter().enumerate() {
        let Some(timeline_name) = RawRrdManifest::get_index_name(start_field) else {
            continue;
        };
        if start_field.metadata().contains_key("rerun:component")
            || !start_field.name().ends_with(":start")
        {
            continue;
        }
        let end_index = schema
            .fields()
            .iter()
            .position(|field| {
                RawRrdManifest::get_index_name(field) == Some(timeline_name)
                    && !field.metadata().contains_key("rerun:component")
                    && field.name().ends_with(":end")
            })
            .ok_or_else(|| {
                rrd_validation_error(
                    format!("RRD timeline {timeline_name} has no manifest end column"),
                    path,
                )
            })?;
        let starts_array = raw_manifest.data.column(start_index);
        let ends_array = raw_manifest.data.column(end_index);
        let (start_type, starts) =
            TimeType::from_arrow_array(starts_array.as_ref()).map_err(|err| {
                rrd_validation_error(
                    format!("RRD timeline {timeline_name} start column is invalid: {err}"),
                    path,
                )
            })?;
        let (end_type, ends) = TimeType::from_arrow_array(ends_array.as_ref()).map_err(|err| {
            rrd_validation_error(
                format!("RRD timeline {timeline_name} end column is invalid: {err}"),
                path,
            )
        })?;
        if start_type != end_type {
            return Err(rrd_validation_error(
                format!("RRD timeline {timeline_name} start/end types disagree"),
                path,
            ));
        }
        for row in 0..chunk_ids.len() {
            match (starts_array.is_valid(row), ends_array.is_valid(row)) {
                (false, false) => {}
                (true, true) => {
                    if chunk_is_static[row] {
                        return Err(rrd_validation_error(
                            format!(
                                "RRD static manifest row {row} claims temporal index {timeline_name}"
                            ),
                            path,
                        ));
                    }
                    if starts[row] > ends[row] {
                        return Err(rrd_validation_error(
                            format!(
                                "RRD timeline {timeline_name} row {row} has inverted range {}..{}",
                                starts[row], ends[row]
                            ),
                            path,
                        ));
                    }
                    let old = expected
                        .get_mut(&chunk_ids[row])
                        .ok_or_else(|| {
                            rrd_validation_error(
                                format!("RRD manifest row {row} has no chunk metadata"),
                                path,
                            )
                        })?
                        .timelines
                        .insert(
                            (timeline_name.to_owned(), timeline_kind(start_type)),
                            (starts[row], ends[row]),
                        );
                    if old.is_some() {
                        return Err(rrd_validation_error(
                            format!("RRD manifest row {row} repeats timeline {timeline_name}"),
                            path,
                        ));
                    }
                }
                _ => {
                    return Err(rrd_validation_error(
                        format!(
                            "RRD timeline {timeline_name} row {row} has mismatched start/end validity"
                        ),
                        path,
                    ));
                }
            }
        }
    }

    for timelines in manifest.temporal_map().values() {
        for components in timelines.values() {
            for (component, chunks) in components {
                for chunk_id in chunks.keys() {
                    let metadata = expected.get_mut(chunk_id).ok_or_else(|| {
                        rrd_validation_error(
                            format!("RRD temporal index references absent chunk {chunk_id}"),
                            path,
                        )
                    })?;
                    metadata.components.insert(component.to_string());
                }
            }
        }
    }
    Ok(expected)
}

fn validate_decoded_chunk_metadata(
    chunk: &re_chunk::Chunk,
    expected_chunks: &std::collections::HashMap<re_chunk::ChunkId, ExpectedChunkMetadata>,
    path: &Path,
) -> Result<(), ImportError> {
    let expected = expected_chunks.get(&chunk.id()).ok_or_else(|| {
        rrd_validation_error(
            format!("Decoded RRD chunk {} has no manifest metadata", chunk.id()),
            path,
        )
    })?;
    if u64::try_from(chunk.num_rows()).unwrap_or(u64::MAX) != expected.num_rows {
        return Err(rrd_validation_error(
            format!(
                "Decoded RRD chunk {} row count disagrees with manifest",
                chunk.id()
            ),
            path,
        ));
    }
    let actual_components = chunk
        .components_identifiers()
        .map(|component| component.to_string())
        .collect::<BTreeSet<_>>();
    if actual_components != expected.components {
        return Err(rrd_validation_error(
            format!(
                "Decoded RRD chunk {} component set disagrees with manifest",
                chunk.id()
            ),
            path,
        ));
    }
    let actual_timelines = chunk
        .timelines()
        .values()
        .map(|column| {
            let range = column.time_range();
            (
                (
                    column.name().to_owned(),
                    timeline_kind(column.timeline().typ()),
                ),
                (range.min().as_i64(), range.max().as_i64()),
            )
        })
        .collect::<BTreeMap<_, _>>();
    if actual_timelines != expected.timelines {
        return Err(rrd_validation_error(
            format!(
                "Decoded RRD chunk {} timeline ranges disagree with manifest",
                chunk.id()
            ),
            path,
        ));
    }
    Ok(())
}

fn timeline_kind(time_type: TimeType) -> TimelineKind {
    match time_type {
        TimeType::Sequence => TimelineKind::Sequence,
        TimeType::TimestampNs => TimelineKind::Timestamp,
        TimeType::DurationNs => TimelineKind::Duration,
    }
}

fn rrd_resource_error(detail: impl Into<String>, path: &Path) -> ImportError {
    ImportError::new(
        ImportErrorCode::ResourceLimitExceeded,
        "The RRD exceeds the safe chunk resource limits.",
        format!("{}\nFile path: {}", detail.into(), path.display()),
    )
}

fn rrd_validation_error(detail: impl Into<String>, path: &Path) -> ImportError {
    ImportError::new(
        ImportErrorCode::OutputValidationFailed,
        "The RRD chunk manifest or payload is corrupt.",
        format!("{}\nFile path: {}", detail.into(), path.display()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use re_chunk::{Chunk, ChunkId, EntityPath, RowId, TimePoint};
    use re_sdk_types::archetypes::SeriesLines;

    #[test]
    fn visualization_categories_follow_verified_rerun_archetypes() {
        assert_eq!(
            archetype_visualization("GridMap"),
            Some(EntityVisualization::Spatial3d)
        );
        assert_eq!(
            archetype_visualization("Transform3D"),
            Some(EntityVisualization::Transform3d)
        );
        assert_eq!(
            archetype_visualization("Points3D"),
            Some(EntityVisualization::Spatial3d)
        );
        assert_eq!(
            archetype_visualization("VoxelGridMap"),
            Some(EntityVisualization::Spatial3d)
        );
        assert_eq!(
            archetype_visualization("Ellipses2D"),
            Some(EntityVisualization::Spatial2d)
        );
        assert_eq!(
            archetype_visualization("EncodedImage"),
            Some(EntityVisualization::Camera)
        );
        assert_eq!(
            archetype_visualization("GeoPoints"),
            Some(EntityVisualization::Map)
        );
        assert_eq!(
            archetype_visualization("CoordinateFrame"),
            None,
            "frame metadata without drawable geometry must not create an empty spatial view"
        );
        assert_eq!(
            archetype_visualization("sensor_msgs.msg.LaserScan"),
            None,
            "raw ROS payloads must not be presented as decoded spatial geometry"
        );
    }

    fn push_varint(bytes: &mut Vec<u8>, mut value: u64) {
        loop {
            let mut byte = (value & 0x7f) as u8;
            value >>= 7;
            if value != 0 {
                byte |= 0x80;
            }
            bytes.push(byte);
            if value == 0 {
                return;
            }
        }
    }

    fn push_bytes_field(bytes: &mut Vec<u8>, field: u8, value: &[u8]) {
        bytes.push((field << 3) | 2);
        push_varint(bytes, value.len() as u64);
        bytes.extend_from_slice(value);
    }

    fn manifest_wire(uncompressed_size: u64) -> Vec<u8> {
        let mut schema = Vec::new();
        push_bytes_field(&mut schema, 1, &[1]);

        let mut dataframe = vec![0x08, 1];
        push_bytes_field(&mut dataframe, 2, &[0]);
        dataframe.extend_from_slice(&[0x18, 2, 0x20]);
        push_varint(&mut dataframe, uncompressed_size);

        let mut manifest = Vec::new();
        push_bytes_field(&mut manifest, 1, &[0x08, 1]);
        push_bytes_field(&mut manifest, 2, &schema);
        push_bytes_field(&mut manifest, 3, &[0; 32]);
        push_bytes_field(&mut manifest, 4, &dataframe);
        manifest
    }

    #[test]
    fn footer_manifest_count_is_bounded_before_prost_decode() {
        let manifest = manifest_wire(1);
        let mut footer = Vec::new();
        for _ in 0..=MAX_RRD_MANIFESTS {
            push_bytes_field(&mut footer, 1, &manifest);
        }
        let error = preflight_rrd_footer_wire(&footer, Path::new("manifest-bomb.rrd"))
            .expect_err("manifest bomb must fail before prost allocation");
        assert_eq!(error.code(), ImportErrorCode::ResourceLimitExceeded);
    }

    #[test]
    fn footer_manifest_decompression_size_is_bounded_before_resize() {
        let manifest = manifest_wire(u64::MAX);
        let mut footer = Vec::new();
        push_bytes_field(&mut footer, 1, &manifest);
        let error = preflight_rrd_footer_wire(&footer, Path::new("manifest-resize-bomb.rrd"))
            .expect_err("manifest resize bomb must fail before decompression");
        assert_eq!(error.code(), ImportErrorCode::ResourceLimitExceeded);
    }

    #[test]
    fn footer_manifest_aggregate_decode_budget_is_bounded_before_decode() {
        let manifest = manifest_wire(MAX_RRD_MANIFEST_ARROW_BYTES);
        let mut footer = Vec::new();
        push_bytes_field(&mut footer, 1, &manifest);
        push_bytes_field(&mut footer, 1, &manifest);
        let error = preflight_rrd_footer_wire(&footer, Path::new("manifest-aggregate-bomb.rrd"))
            .expect_err("aggregate manifest decode budget must be checked before decoding");
        assert_eq!(error.code(), ImportErrorCode::ResourceLimitExceeded);
    }

    #[test]
    fn raw_manifest_rows_are_bounded_before_index_expansion() {
        validate_raw_manifest_row_count([MAX_RRD_CHUNKS], Path::new("bounded.rrd"))
            .expect("the exact manifest row limit is accepted");
        let error = validate_raw_manifest_row_count(
            [MAX_RRD_CHUNKS, 1],
            Path::new("manifest-row-bomb.rrd"),
        )
        .expect_err("manifest rows beyond the service budget must be rejected");
        assert_eq!(error.code(), ImportErrorCode::ResourceLimitExceeded);
    }

    #[test]
    fn duplicate_chunk_spans_are_rejected_before_payload_reads() {
        let mut spans = vec![(128, 256), (128, 256)];
        let error = validate_rrd_chunk_span_set(&mut spans, Path::new("duplicate-span.rrd"))
            .expect_err("duplicate referenced payload spans must be rejected");
        assert_eq!(error.code(), ImportErrorCode::OutputValidationFailed);
    }

    #[test]
    fn repeated_static_rows_are_verified_from_raw_manifest()
    -> Result<(), Box<dyn std::error::Error>> {
        let chunks = ["first", "second"]
            .into_iter()
            .map(|name| {
                Chunk::builder("series")
                    .with_archetype(
                        RowId::new(),
                        TimePoint::STATIC,
                        &SeriesLines::new().with_names([name]),
                    )
                    .build()
            })
            .collect::<Result<Vec<_>, _>>()?;
        let raw = RawRrdManifest::build_in_memory_from_chunks(
            re_log_types::StoreId::empty_recording(),
            chunks.iter(),
        )?;
        let manifest = RrdManifest::try_new(&raw)?;

        let latest_static_ids = manifest
            .static_map()
            .values()
            .flat_map(|components| components.values())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            latest_static_ids.len(),
            1,
            "the query index intentionally keeps only the latest repeated static chunk"
        );

        let expected =
            expected_chunk_metadata(&raw, &manifest, Path::new("repeated-static-manifest.rrd"))?;
        assert_eq!(expected.len(), chunks.len());
        for chunk in &chunks {
            validate_decoded_chunk_metadata(
                chunk,
                &expected,
                Path::new("repeated-static-manifest.rrd"),
            )?;
        }
        Ok(())
    }

    #[test]
    fn empty_blueprint_chunk_is_not_mistaken_for_missing_metadata()
    -> Result<(), Box<dyn std::error::Error>> {
        let chunk = Chunk::empty(ChunkId::new(), EntityPath::from("empty-blueprint"));
        let raw = RawRrdManifest::build_in_memory_from_chunks(
            re_log_types::StoreId::new(StoreKind::Blueprint, "test", "test"),
            std::iter::once(&chunk),
        )?;
        let manifest = RrdManifest::try_new(&raw)?;
        let expected =
            expected_chunk_metadata(&raw, &manifest, Path::new("empty-blueprint-manifest.rrd"))?;

        validate_decoded_chunk_metadata(
            &chunk,
            &expected,
            Path::new("empty-blueprint-manifest.rrd"),
        )?;
        Ok(())
    }

    #[test]
    fn rrd_decode_budget_is_service_bounded() {
        assert_eq!(MAX_RRD_CHUNKS, 65_536);
        assert_eq!(MAX_RRD_CHUNK_COMPRESSED_BYTES, 64 * 1024 * 1024);
        assert_eq!(MAX_RRD_CHUNK_UNCOMPRESSED_BYTES, 64 * 1024 * 1024);
        assert_eq!(MAX_RRD_TOTAL_COMPRESSED_BYTES, 2 * 1024 * 1024 * 1024);
        assert_eq!(MAX_RRD_TOTAL_UNCOMPRESSED_BYTES, 2 * 1024 * 1024 * 1024);
        assert_eq!(MAX_RRD_FOOTER_DECODE_BUDGET_BYTES, 128 * 1024 * 1024);
    }
}
