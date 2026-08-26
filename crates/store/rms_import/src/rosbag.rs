use std::collections::BTreeSet;
use std::io::{Read as _, Seek as _, Write as _};
use std::path::{Path, PathBuf};

use crate::external_tools::convert_rosbag_to_mcap;
use crate::{ImportCancellation, ImportError, ImportErrorCode};

const MAX_ARCHIVE_ENTRIES: usize = 4096;
const MAX_CENTRAL_DIRECTORY_BYTES: u64 = 32 * 1024 * 1024;
const HARD_MAX_ARCHIVE_EXPANDED_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MIN_ARCHIVE_EXPANDED_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ARCHIVE_COMPRESSION_RATIO: u64 = 1000;
const MAX_METADATA_BYTES: u64 = 1024 * 1024;
const EXTRACTION_BUFFER_BYTES: usize = 1024 * 1024;

pub(crate) struct PreparedRosbag {
    _workspace: tempfile::TempDir,
    pub mcap_paths: Vec<PathBuf>,
    pub warnings: Vec<String>,
}

pub(crate) fn prepare_rosbag(
    source: &Path,
    max_output_bytes: u64,
    cancellation: Option<&ImportCancellation>,
) -> Result<PreparedRosbag, ImportError> {
    validate_zip_magic(source)?;
    validate_zip_directory_bounds(source)?;
    let expanded_limit = max_output_bytes
        .saturating_mul(4)
        .clamp(MIN_ARCHIVE_EXPANDED_BYTES, HARD_MAX_ARCHIVE_EXPANDED_BYTES);
    let workspace = tempfile::tempdir().map_err(|err| {
        ImportError::new(
            ImportErrorCode::Io,
            "The import worker could not create ROS bag workspace.",
            format!("Failed to create ROS bag workspace: {err}"),
        )
    })?;
    let extract_root = workspace.path().join("bag");
    std::fs::create_dir(&extract_root).map_err(|err| {
        ImportError::new(
            ImportErrorCode::Io,
            "The import worker could not create ROS bag workspace.",
            format!("Failed to create ROS bag extraction root: {err}"),
        )
    })?;

    let file = std::fs::File::open(source).map_err(|err| {
        ImportError::new(
            ImportErrorCode::Io,
            "The ROS 2 bag archive could not be read.",
            format!(
                "Failed to open ROS bag ZIP: {err}\nFile path: {}",
                source.display()
            ),
        )
    })?;
    let mut archive = zip::ZipArchive::new(std::io::BufReader::new(file)).map_err(|err| {
        ImportError::new(
            ImportErrorCode::ValidationFailed,
            "The ROS 2 bag is not a valid ZIP archive.",
            format!(
                "Failed to parse ROS bag ZIP: {err}\nFile path: {}",
                source.display()
            ),
        )
    })?;
    if archive.is_empty() || archive.len() > MAX_ARCHIVE_ENTRIES {
        return Err(ImportError::new(
            ImportErrorCode::ResourceLimitExceeded,
            "The ROS 2 bag archive has an unsupported number of entries.",
            format!(
                "ZIP contains {} entries; maximum is {MAX_ARCHIVE_ENTRIES}",
                archive.len()
            ),
        ));
    }

    let mut extracted_paths = BTreeSet::new();
    let mut metadata_paths = Vec::new();
    let mut mcap_paths = Vec::new();
    let mut db3_paths = Vec::new();
    let mut total_declared = 0_u64;
    let mut total_extracted = 0_u64;
    let mut extraction_buffer = vec![0_u8; EXTRACTION_BUFFER_BYTES];
    for index in 0..archive.len() {
        check_cancelled(cancellation)?;
        let mut entry = archive.by_index(index).map_err(|err| {
            ImportError::new(
                ImportErrorCode::ValidationFailed,
                "A ROS 2 bag archive entry could not be read.",
                format!("Failed to open ZIP entry {index}: {err}"),
            )
        })?;
        let enclosed = entry.enclosed_name().ok_or_else(|| {
            ImportError::new(
                ImportErrorCode::ValidationFailed,
                "The ROS 2 bag archive contains an unsafe path.",
                format!("ZIP path traversal entry rejected: {:?}", entry.name()),
            )
        })?;
        if enclosed
            .components()
            .any(|component| component.as_os_str() == "_rms_converted")
        {
            return Err(ImportError::new(
                ImportErrorCode::ValidationFailed,
                "The ROS 2 bag archive contains a reserved path.",
                format!(
                    "ZIP entry uses reserved importer path: {}",
                    enclosed.display()
                ),
            ));
        }
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err(ImportError::new(
                ImportErrorCode::ValidationFailed,
                "The ROS 2 bag archive contains a symbolic link.",
                format!("ZIP symbolic link entry rejected: {}", enclosed.display()),
            ));
        }
        total_declared = total_declared
            .checked_add(entry.size())
            .ok_or_else(|| archive_limit_error("ZIP expanded-size counter overflow"))?;
        if total_declared > expanded_limit {
            return Err(archive_limit_error(format!(
                "ZIP declares {total_declared} expanded bytes; request-linked limit is {expanded_limit}"
            )));
        }
        if entry.compressed_size() > 0
            && entry.size() > 1024 * 1024
            && entry.size() / entry.compressed_size() > MAX_ARCHIVE_COMPRESSION_RATIO
        {
            return Err(archive_limit_error(format!(
                "ZIP entry compression ratio exceeds {MAX_ARCHIVE_COMPRESSION_RATIO}:1: {}",
                enclosed.display()
            )));
        }

        let output = extract_root.join(&enclosed);
        if !extracted_paths.insert(enclosed.clone()) {
            return Err(ImportError::new(
                ImportErrorCode::ValidationFailed,
                "The ROS 2 bag archive contains duplicate paths.",
                format!("Duplicate ZIP entry: {}", enclosed.display()),
            ));
        }
        if entry.is_dir() {
            std::fs::create_dir_all(&output).map_err(|err| extraction_io_error(&output, err))?;
            continue;
        }
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent).map_err(|err| extraction_io_error(parent, err))?;
        }
        let mut output_file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&output)
            .map_err(|err| extraction_io_error(&output, err))?;
        let declared_size = entry.size();
        let mut copied = 0_u64;
        loop {
            check_cancelled(cancellation)?;
            let count = entry.read(&mut extraction_buffer).map_err(|err| {
                ImportError::new(
                    ImportErrorCode::ValidationFailed,
                    "A ROS 2 bag archive entry is corrupt.",
                    format!(
                        "ZIP entry read/CRC validation failed: {err}\nEntry: {}",
                        enclosed.display()
                    ),
                )
            })?;
            if count == 0 {
                break;
            }
            let count_u64 = u64::try_from(count).map_err(|_conversion_error| {
                archive_limit_error("ZIP extraction byte count does not fit u64")
            })?;
            copied = copied
                .checked_add(count_u64)
                .ok_or_else(|| archive_limit_error("ZIP entry byte counter overflow"))?;
            total_extracted = total_extracted
                .checked_add(count_u64)
                .ok_or_else(|| archive_limit_error("ZIP extracted-size counter overflow"))?;
            if copied > declared_size || total_extracted > expanded_limit {
                return Err(archive_limit_error(format!(
                    "ZIP extracted {total_extracted} bytes; request-linked limit is {expanded_limit}"
                )));
            }
            output_file
                .write_all(&extraction_buffer[..count])
                .map_err(|err| extraction_io_error(&output, err))?;
        }
        output_file
            .flush()
            .map_err(|err| extraction_io_error(&output, err))?;
        if copied != declared_size {
            return Err(ImportError::new(
                ImportErrorCode::ValidationFailed,
                "A ROS 2 bag archive entry is truncated.",
                format!(
                    "ZIP entry {} declared {} bytes but extracted {copied}",
                    enclosed.display(),
                    declared_size
                ),
            ));
        }
        match output.file_name().and_then(|name| name.to_str()) {
            Some("metadata.yaml") => metadata_paths.push(output),
            _ => match output
                .extension()
                .and_then(|extension| extension.to_str())
                .map(str::to_ascii_lowercase)
                .as_deref()
            {
                Some("mcap") => mcap_paths.push(output),
                Some("db3") => db3_paths.push(output),
                _ => {}
            },
        }
    }
    if metadata_paths.len() != 1 {
        return Err(ImportError::new(
            ImportErrorCode::ValidationFailed,
            "A ROS 2 bag ZIP must contain exactly one metadata.yaml.",
            format!("Found {} metadata.yaml files", metadata_paths.len()),
        ));
    }
    let metadata_path = &metadata_paths[0];
    let metadata = parse_metadata(&read_small_text(metadata_path)?)?;
    let bag_root = metadata_path.parent().ok_or_else(|| {
        ImportError::new(
            ImportErrorCode::ValidationFailed,
            "The ROS 2 bag metadata path is invalid.",
            format!("metadata.yaml has no parent: {}", metadata_path.display()),
        )
    })?;
    let ordered_segments = validate_declared_segments(
        bag_root,
        &metadata.relative_file_paths,
        &mcap_paths,
        &db3_paths,
    )?;

    let (mcap_paths, warnings) = if metadata.storage_identifier == "mcap" {
        if ordered_segments
            .iter()
            .any(|path| !has_extension(path, "mcap"))
        {
            return Err(ImportError::new(
                ImportErrorCode::ValidationFailed,
                "ROS 2 metadata storage and segment extensions disagree.",
                "storage_identifier=mcap requires only listed .mcap segments",
            ));
        }
        (ordered_segments, Vec::new())
    } else if metadata.storage_identifier == "sqlite3" {
        if ordered_segments
            .iter()
            .any(|path| !has_extension(path, "db3"))
        {
            return Err(ImportError::new(
                ImportErrorCode::ValidationFailed,
                "ROS 2 metadata storage and segment extensions disagree.",
                "storage_identifier=sqlite3 requires only listed .db3 segments",
            ));
        }
        check_cancelled(cancellation)?;
        let converted = workspace.path().join("_rms_converted");
        convert_rosbag_to_mcap(bag_root, &converted, expanded_limit, cancellation)?;
        check_cancelled(cancellation)?;
        let mut converted_mcaps = Vec::new();
        collect_files_with_extension(&converted, "mcap", 0, &mut converted_mcaps, cancellation)?;
        converted_mcaps.sort();
        if converted_mcaps.is_empty() {
            return Err(ImportError::new(
                ImportErrorCode::ConversionFailed,
                "rosbags-convert produced no MCAP data.",
                format!("No .mcap files found under {}", converted.display()),
            ));
        }
        let total = converted_mcaps.iter().try_fold(0_u64, |total, path| {
            let size = path
                .metadata()
                .map_err(|err| extraction_io_error(path, err))?
                .len();
            total
                .checked_add(size)
                .ok_or_else(|| archive_limit_error("Converted MCAP size overflow"))
        })?;
        if total > expanded_limit {
            return Err(archive_limit_error(format!(
                "rosbags-convert produced {total} bytes; request-linked limit is {expanded_limit}"
            )));
        }
        (
            converted_mcaps,
            vec!["Converted sqlite3 ROS 2 bag storage to MCAP with the configured rosbags-convert worker tool".to_owned()],
        )
    } else {
        return Err(ImportError::new(
            ImportErrorCode::UnsupportedFormat,
            "The ROS 2 bag storage type is not supported.",
            format!(
                "Unsupported storage_identifier {:?}; expected mcap or sqlite3",
                metadata.storage_identifier
            ),
        ));
    };

    for path in &mcap_paths {
        validate_mcap_magic(path)?;
    }
    Ok(PreparedRosbag {
        _workspace: workspace,
        mcap_paths,
        warnings,
    })
}

pub(crate) fn validate_zip_magic(path: &Path) -> Result<(), ImportError> {
    let prefix = read_prefix(path, 4)?;
    if matches!(
        prefix.as_slice(),
        b"PK\x03\x04" | b"PK\x05\x06" | b"PK\x07\x08"
    ) {
        Ok(())
    } else {
        Err(ImportError::new(
            ImportErrorCode::ValidationFailed,
            "The declared ROS 2 bag ZIP does not match the file contents.",
            format!("ZIP magic mismatch: {prefix:02x?}"),
        ))
    }
}

fn validate_zip_directory_bounds(path: &Path) -> Result<(), ImportError> {
    const EOCD_MIN_BYTES: u64 = 22;
    const MAX_COMMENT_BYTES: u64 = u16::MAX as u64;
    const ZIP64_LOCATOR_BYTES: u64 = 20;
    const ZIP64_EOCD_MIN_BYTES: u64 = 56;

    let mut file = std::fs::File::open(path).map_err(|err| extraction_io_error(path, err))?;
    let file_size = file
        .metadata()
        .map_err(|err| extraction_io_error(path, err))?
        .len();
    if file_size < EOCD_MIN_BYTES {
        return Err(metadata_error(
            "ZIP is too small to contain an end-of-directory record",
        ));
    }
    let tail_size = file_size.min(EOCD_MIN_BYTES + MAX_COMMENT_BYTES);
    file.seek(std::io::SeekFrom::Start(file_size - tail_size))
        .map_err(|err| extraction_io_error(path, err))?;
    let mut tail = vec![
        0_u8;
        usize::try_from(tail_size).map_err(|err| {
            archive_limit_error(format!("ZIP tail size does not fit memory: {err}"))
        })?
    ];
    file.read_exact(&mut tail)
        .map_err(|err| extraction_io_error(path, err))?;
    let eocd_index = (0..=tail.len().saturating_sub(22))
        .rev()
        .find(|index| {
            tail.get(*index..*index + 4) == Some(b"PK\x05\x06")
                && read_u16_le(&tail, *index + 20)
                    .is_some_and(|comment| *index + 22 + usize::from(comment) == tail.len())
        })
        .ok_or_else(|| metadata_error("ZIP end-of-central-directory record was not found"))?;
    let eocd_offset = file_size - tail_size
        + u64::try_from(eocd_index).map_err(|err| {
            archive_limit_error(format!("ZIP EOCD offset does not fit u64: {err}"))
        })?;
    let disk = read_u16_le(&tail, eocd_index + 4)
        .ok_or_else(|| metadata_error("ZIP EOCD disk field is truncated"))?;
    let central_disk = read_u16_le(&tail, eocd_index + 6)
        .ok_or_else(|| metadata_error("ZIP EOCD central disk field is truncated"))?;
    let entries_on_disk = read_u16_le(&tail, eocd_index + 8)
        .ok_or_else(|| metadata_error("ZIP EOCD entry count is truncated"))?;
    let entries_total = read_u16_le(&tail, eocd_index + 10)
        .ok_or_else(|| metadata_error("ZIP EOCD total entry count is truncated"))?;
    let central_size = read_u32_le(&tail, eocd_index + 12)
        .ok_or_else(|| metadata_error("ZIP EOCD central size is truncated"))?;
    let central_offset = read_u32_le(&tail, eocd_index + 16)
        .ok_or_else(|| metadata_error("ZIP EOCD central offset is truncated"))?;

    let is_zip64 =
        entries_total == u16::MAX || central_size == u32::MAX || central_offset == u32::MAX;
    let (entries, central_size, central_offset) = if is_zip64 {
        if eocd_offset < ZIP64_LOCATOR_BYTES {
            return Err(metadata_error("ZIP64 locator offset underflow"));
        }
        file.seek(std::io::SeekFrom::Start(eocd_offset - ZIP64_LOCATOR_BYTES))
            .map_err(|err| extraction_io_error(path, err))?;
        let mut locator = [0_u8; 20];
        file.read_exact(&mut locator)
            .map_err(|err| extraction_io_error(path, err))?;
        if locator[..4] != *b"PK\x06\x07" {
            return Err(metadata_error("ZIP64 locator signature is missing"));
        }
        let zip64_offset = read_u64_le_bytes(&locator, 8)
            .ok_or_else(|| metadata_error("ZIP64 EOCD offset is truncated"))?;
        let zip64 = read_exact_at(&mut file, zip64_offset, ZIP64_EOCD_MIN_BYTES, path)?;
        if zip64[..4] != *b"PK\x06\x06" {
            return Err(metadata_error("ZIP64 EOCD signature is missing"));
        }
        let record_size = read_u64_le_bytes(&zip64, 4)
            .ok_or_else(|| metadata_error("ZIP64 EOCD size is truncated"))?;
        if record_size < 44 || record_size > MAX_CENTRAL_DIRECTORY_BYTES {
            return Err(archive_limit_error(format!(
                "ZIP64 EOCD record size {record_size} is outside safe limits"
            )));
        }
        let entries_disk = read_u64_le_bytes(&zip64, 24)
            .ok_or_else(|| metadata_error("ZIP64 entry count is truncated"))?;
        let entries = read_u64_le_bytes(&zip64, 32)
            .ok_or_else(|| metadata_error("ZIP64 total entry count is truncated"))?;
        let central_size = read_u64_le_bytes(&zip64, 40)
            .ok_or_else(|| metadata_error("ZIP64 central size is truncated"))?;
        let central_offset = read_u64_le_bytes(&zip64, 48)
            .ok_or_else(|| metadata_error("ZIP64 central offset is truncated"))?;
        if entries_disk != entries {
            return Err(metadata_error(
                "Multi-disk ZIP64 archives are not supported",
            ));
        }
        (entries, central_size, central_offset)
    } else {
        if disk != 0 || central_disk != 0 || entries_on_disk != entries_total {
            return Err(metadata_error("Multi-disk ZIP archives are not supported"));
        }
        (
            u64::from(entries_total),
            u64::from(central_size),
            u64::from(central_offset),
        )
    };
    if entries == 0 || entries > MAX_ARCHIVE_ENTRIES as u64 {
        return Err(archive_limit_error(format!(
            "ZIP central directory declares {entries} entries; maximum is {MAX_ARCHIVE_ENTRIES}"
        )));
    }
    if central_size == 0 || central_size > MAX_CENTRAL_DIRECTORY_BYTES {
        return Err(archive_limit_error(format!(
            "ZIP central directory is {central_size} bytes; maximum is {MAX_CENTRAL_DIRECTORY_BYTES}"
        )));
    }
    let central_end = central_offset
        .checked_add(central_size)
        .ok_or_else(|| archive_limit_error("ZIP central-directory span overflow"))?;
    if central_end > file_size || central_end > eocd_offset {
        return Err(metadata_error(format!(
            "ZIP central-directory span {central_offset}..{central_end} exceeds file bounds"
        )));
    }
    Ok(())
}

fn read_exact_at(
    file: &mut std::fs::File,
    offset: u64,
    len: u64,
    path: &Path,
) -> Result<Vec<u8>, ImportError> {
    file.seek(std::io::SeekFrom::Start(offset))
        .map_err(|err| extraction_io_error(path, err))?;
    let mut bytes = vec![
        0_u8;
        usize::try_from(len).map_err(|err| {
            archive_limit_error(format!("ZIP record length does not fit memory: {err}"))
        })?
    ];
    file.read_exact(&mut bytes)
        .map_err(|err| extraction_io_error(path, err))?;
    Ok(bytes)
}

fn read_u16_le(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn read_u32_le(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn read_u64_le_bytes(bytes: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        bytes.get(offset..offset + 8)?.try_into().ok()?,
    ))
}

fn validate_mcap_magic(path: &Path) -> Result<(), ImportError> {
    let prefix = read_prefix(path, mcap::MAGIC.len())?;
    if prefix == mcap::MAGIC {
        Ok(())
    } else {
        Err(ImportError::new(
            ImportErrorCode::ValidationFailed,
            "A ROS 2 bag segment is not valid MCAP data.",
            format!("MCAP magic mismatch\nFile path: {}", path.display()),
        ))
    }
}

fn read_prefix(path: &Path, len: usize) -> Result<Vec<u8>, ImportError> {
    let mut file = std::fs::File::open(path).map_err(|err| extraction_io_error(path, err))?;
    let mut prefix = vec![0_u8; len];
    file.read_exact(&mut prefix).map_err(|err| {
        ImportError::new(
            ImportErrorCode::ValidationFailed,
            "The archive or bag segment is truncated.",
            format!(
                "Failed to read file signature: {err}\nFile path: {}",
                path.display()
            ),
        )
    })?;
    Ok(prefix)
}

fn read_small_text(path: &Path) -> Result<String, ImportError> {
    let size = path
        .metadata()
        .map_err(|err| extraction_io_error(path, err))?
        .len();
    if size > MAX_METADATA_BYTES {
        return Err(archive_limit_error(format!(
            "metadata.yaml is {size} bytes; maximum is {MAX_METADATA_BYTES}"
        )));
    }
    std::fs::read_to_string(path).map_err(|err| {
        ImportError::new(
            ImportErrorCode::ValidationFailed,
            "ROS 2 metadata.yaml must be valid UTF-8 text.",
            format!(
                "Failed to read metadata.yaml: {err}\nFile path: {}",
                path.display()
            ),
        )
    })
}

struct RosbagMetadata {
    storage_identifier: String,
    relative_file_paths: Vec<String>,
}

fn parse_metadata(metadata: &str) -> Result<RosbagMetadata, ImportError> {
    let lines = metadata.lines().collect::<Vec<_>>();
    if lines.len() > 100_000 {
        return Err(archive_limit_error("metadata.yaml has too many lines"));
    }
    if lines.iter().any(|line| line.contains('\t')) {
        return Err(metadata_error(
            "Tabs are not accepted in metadata.yaml indentation",
        ));
    }

    let mut storage_identifier = None;
    let mut relative_paths_line = None;
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix("storage_identifier:") {
            if storage_identifier.is_some() {
                return Err(metadata_error("Duplicate storage_identifier keys"));
            }
            storage_identifier = Some(parse_metadata_scalar(value, "storage_identifier")?);
        }
        if trimmed == "relative_file_paths:" && relative_paths_line.replace(index).is_some() {
            return Err(metadata_error("Duplicate relative_file_paths keys"));
        }
    }
    let storage_identifier = storage_identifier
        .ok_or_else(|| metadata_error("Missing storage_identifier in metadata.yaml"))?;
    let list_index = relative_paths_line
        .ok_or_else(|| metadata_error("Missing relative_file_paths in metadata.yaml"))?;
    let base_indent = leading_spaces(lines[list_index]);
    if base_indent > 64 {
        return Err(metadata_error(
            "relative_file_paths nesting exceeds 64 spaces",
        ));
    }

    let mut relative_file_paths = Vec::new();
    for line in &lines[list_index + 1..] {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = leading_spaces(line);
        let Some(value) = trimmed.strip_prefix("- ") else {
            if indent <= base_indent {
                break;
            }
            return Err(metadata_error(
                "relative_file_paths must be a flat YAML sequence of scalar paths",
            ));
        };
        if indent < base_indent {
            break;
        }
        if relative_file_paths.len() >= 128 {
            return Err(archive_limit_error(
                "metadata.yaml lists more than 128 bag segments",
            ));
        }
        let value = parse_metadata_scalar(value, "relative_file_paths item")?;
        validate_relative_segment(&value)?;
        if relative_file_paths.contains(&value) {
            return Err(metadata_error(format!(
                "Duplicate relative_file_paths item: {value:?}"
            )));
        }
        relative_file_paths.push(value);
    }
    if relative_file_paths.is_empty() {
        return Err(metadata_error("relative_file_paths is empty"));
    }

    Ok(RosbagMetadata {
        storage_identifier,
        relative_file_paths,
    })
}

fn parse_metadata_scalar(value: &str, field: &str) -> Result<String, ImportError> {
    let value = value.trim();
    if value.is_empty() || value.starts_with(['&', '*', '!', '[', '{']) || value.contains(" #") {
        return Err(metadata_error(format!(
            "Unsupported YAML scalar in {field}: {value:?}"
        )));
    }
    let value = if let Some(quoted) = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
        if quoted.contains(['\\', '"']) {
            return Err(metadata_error(format!(
                "Escaped quoted scalars are not supported in {field}"
            )));
        }
        quoted
    } else if let Some(quoted) = value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')) {
        if quoted.contains('\'') {
            return Err(metadata_error(format!(
                "Escaped quoted scalars are not supported in {field}"
            )));
        }
        quoted
    } else {
        value
    };
    if value.is_empty() {
        return Err(metadata_error(format!("Empty scalar in {field}")));
    }
    Ok(value.to_owned())
}

fn leading_spaces(line: &str) -> usize {
    line.bytes().take_while(|byte| *byte == b' ').count()
}

fn validate_relative_segment(value: &str) -> Result<(), ImportError> {
    if value.contains('\\')
        || value.starts_with('/')
        || value.contains(':')
        || value
            .split('/')
            .any(|component| component.is_empty() || matches!(component, "." | ".."))
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '_' | '-' | '.'))
    {
        return Err(metadata_error(format!(
            "Unsafe relative_file_paths item: {value:?}"
        )));
    }
    Ok(())
}

fn validate_declared_segments(
    bag_root: &Path,
    declared: &[String],
    mcap_paths: &[PathBuf],
    db3_paths: &[PathBuf],
) -> Result<Vec<PathBuf>, ImportError> {
    let mut actual = std::collections::BTreeMap::new();
    for path in mcap_paths.iter().chain(db3_paths) {
        let relative = path.strip_prefix(bag_root).map_err(|_strip_error| {
            ImportError::new(
                ImportErrorCode::ValidationFailed,
                "The ROS 2 bag contains a segment outside the metadata directory.",
                format!(
                    "Unlisted bag segment is outside metadata root\nFile path: {}",
                    path.display()
                ),
            )
        })?;
        let relative = relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        if actual.insert(relative.clone(), path.clone()).is_some() {
            return Err(metadata_error(format!(
                "Duplicate extracted bag segment: {relative:?}"
            )));
        }
    }
    if actual.len() != declared.len()
        || declared
            .iter()
            .any(|relative| !actual.contains_key(relative))
    {
        let actual_paths = actual.keys().cloned().collect::<Vec<_>>();
        return Err(ImportError::new(
            ImportErrorCode::ValidationFailed,
            "ROS 2 metadata must list every bag segment exactly once.",
            format!("relative_file_paths mismatch; declared={declared:?}, actual={actual_paths:?}"),
        ));
    }
    declared
        .iter()
        .map(|relative| {
            actual.get(relative).cloned().ok_or_else(|| {
                metadata_error(format!("Missing declared bag segment: {relative:?}"))
            })
        })
        .collect()
}

fn has_extension(path: &Path, expected: &str) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case(expected))
}

fn metadata_error(detail: impl Into<String>) -> ImportError {
    ImportError::new(
        ImportErrorCode::ValidationFailed,
        "The ROS 2 metadata.yaml structure is invalid.",
        detail,
    )
}

fn collect_files_with_extension(
    directory: &Path,
    extension: &str,
    depth: usize,
    output: &mut Vec<PathBuf>,
    cancellation: Option<&ImportCancellation>,
) -> Result<(), ImportError> {
    check_cancelled(cancellation)?;
    if depth > 8 || output.len() > 128 {
        return Err(archive_limit_error(
            "Converted ROS bag directory is too deep or wide",
        ));
    }
    for entry in std::fs::read_dir(directory).map_err(|err| extraction_io_error(directory, err))? {
        let entry = entry.map_err(|err| extraction_io_error(directory, err))?;
        let file_type = entry
            .file_type()
            .map_err(|err| extraction_io_error(&entry.path(), err))?;
        if file_type.is_symlink() {
            return Err(ImportError::new(
                ImportErrorCode::ValidationFailed,
                "The converted ROS 2 bag contains a symbolic link.",
                format!("Converted symlink rejected: {}", entry.path().display()),
            ));
        }
        if file_type.is_dir() {
            collect_files_with_extension(
                &entry.path(),
                extension,
                depth + 1,
                output,
                cancellation,
            )?;
        } else if file_type.is_file()
            && entry
                .path()
                .extension()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case(extension))
        {
            if output.len() >= 128 {
                return Err(archive_limit_error(
                    "Converted ROS bag contains more than 128 MCAP segments",
                ));
            }
            output.push(entry.path());
        }
    }
    Ok(())
}

fn archive_limit_error(detail: impl Into<String>) -> ImportError {
    ImportError::new(
        ImportErrorCode::ResourceLimitExceeded,
        "The ROS 2 bag archive exceeds the safe extraction limits.",
        detail,
    )
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "used directly from map_err closures that own the I/O error"
)]
fn extraction_io_error(path: &Path, err: std::io::Error) -> ImportError {
    ImportError::new(
        ImportErrorCode::Io,
        "The ROS 2 bag could not be extracted.",
        format!(
            "Archive extraction failed: {err}\nFile path: {}",
            path.display()
        ),
    )
}

fn check_cancelled(cancellation: Option<&ImportCancellation>) -> Result<(), ImportError> {
    if cancellation.is_some_and(ImportCancellation::is_cancelled) {
        Err(ImportError::cancelled())
    } else {
        Ok(())
    }
}
