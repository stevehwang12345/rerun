use std::path::Path;

use re_log_types::{EntityPath, TimePoint, TimeType};

use crate::external_tools::transcode_to_mp4;
use crate::{ImportCancellation, ImportError, ImportErrorCode, RecordingWriter};

pub(crate) struct VideoImportSummary {
    pub default_timeline: String,
    pub topics: Vec<String>,
    pub warnings: Vec<String>,
}

pub(crate) fn import_video(
    source: &Path,
    original_file_name: &str,
    extension: &str,
    max_output_bytes: u64,
    cancellation: Option<&ImportCancellation>,
    writer: &mut RecordingWriter<'_>,
) -> Result<VideoImportSummary, ImportError> {
    validate_video_magic(source, extension)?;
    let transcode_dir = tempfile::tempdir().map_err(|err| {
        ImportError::new(
            ImportErrorCode::Io,
            "The import worker could not create video workspace.",
            format!("Failed to create video transcode directory: {err}"),
        )
    })?;
    let transcoded_path = transcode_dir.path().join("normalized.mp4");
    let mp4_path = if extension == "mp4" {
        source.to_path_buf()
    } else {
        transcode_to_mp4(source, &transcoded_path, max_output_bytes, cancellation)?;
        validate_iso_bmff(&transcoded_path)?;
        transcoded_path
    };

    let stem = Path::new(original_file_name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(sanitize_segment)
        .unwrap_or_else(|| "recording".to_owned());
    let entity_path = EntityPath::parse_forgiving(&format!("video/{stem}"));
    let config = re_mp4_reader::Mp4Config {
        mode: re_mp4_reader::Mode::Asset {
            timepoint: TimePoint::STATIC,
        },
        timeline_name: "video".into(),
        timeline_type: TimeType::DurationNs,
    };
    let chunks = re_mp4_reader::load_mp4(&mp4_path, &config, &entity_path).map_err(|err| {
        ImportError::new(
            ImportErrorCode::ValidationFailed,
            "The video is not a supported Replay-compatible MP4 stream.",
            format!(
                "re_mp4_reader rejected video: {err}\nFile path: {}",
                mp4_path.display()
            ),
        )
    })?;
    let mut chunk_count = 0_usize;
    for chunk in chunks {
        let chunk = chunk.map_err(|err| {
            ImportError::new(
                ImportErrorCode::ConversionFailed,
                "The video could not be encoded into Rerun chunks.",
                format!(
                    "Video chunk conversion failed: {err}\nFile path: {}",
                    mp4_path.display()
                ),
            )
        })?;
        writer.append_chunk(&chunk)?;
        chunk_count += 1;
    }
    if chunk_count < 2 {
        return Err(ImportError::new(
            ImportErrorCode::ValidationFailed,
            "The video contains no readable frame timeline.",
            format!("Video importer emitted only {chunk_count} chunk(s)"),
        ));
    }

    Ok(VideoImportSummary {
        default_timeline: "video".to_owned(),
        topics: vec![entity_path.to_string()],
        warnings: if extension == "mp4" {
            Vec::new()
        } else {
            vec![format!(
                "Transcoded {extension} video to MP4 for Rerun playback"
            )]
        },
    })
}

pub(crate) fn validate_video_magic(source: &Path, extension: &str) -> Result<(), ImportError> {
    match extension {
        "mp4" | "mov" => validate_iso_bmff(source),
        "webm" => {
            let prefix = read_prefix(source, 4)?;
            if prefix == [0x1A, 0x45, 0xDF, 0xA3] {
                Ok(())
            } else {
                Err(format_mismatch("WebM EBML", &prefix))
            }
        }
        _ => Err(ImportError::new(
            ImportErrorCode::UnsupportedFormat,
            "Only MP4, MOV, and WebM video files are supported.",
            format!("Unsupported video extension: {extension:?}"),
        )),
    }
}

fn validate_iso_bmff(source: &Path) -> Result<(), ImportError> {
    let prefix = read_prefix(source, 12)?;
    if prefix.get(4..8) == Some(b"ftyp") {
        Ok(())
    } else {
        Err(format_mismatch("ISO-BMFF ftyp", &prefix))
    }
}

fn read_prefix(source: &Path, len: usize) -> Result<Vec<u8>, ImportError> {
    use std::io::Read as _;

    let mut file = std::fs::File::open(source).map_err(|err| {
        ImportError::new(
            ImportErrorCode::Io,
            "The video file could not be read.",
            format!(
                "Failed to open video: {err}\nFile path: {}",
                source.display()
            ),
        )
    })?;
    let mut prefix = vec![0_u8; len];
    file.read_exact(&mut prefix).map_err(|err| {
        ImportError::new(
            ImportErrorCode::ValidationFailed,
            "The video file is truncated.",
            format!(
                "Failed to read video signature: {err}\nFile path: {}",
                source.display()
            ),
        )
    })?;
    Ok(prefix)
}

fn format_mismatch(expected: &str, actual: &[u8]) -> ImportError {
    ImportError::new(
        ImportErrorCode::ValidationFailed,
        "The declared video format does not match the file contents.",
        format!("Expected {expected} signature, got {actual:02x?}"),
    )
}

fn sanitize_segment(value: &str) -> String {
    let value = value
        .chars()
        .map(|character| {
            if character.is_alphanumeric() || matches!(character, '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .take(64)
        .collect::<String>();
    if value.is_empty() {
        "recording".to_owned()
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::validate_video_magic;

    #[test]
    fn extension_and_video_magic_must_agree() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"not a video at all").unwrap();
        let error = validate_video_magic(file.path(), "mp4").unwrap_err();
        assert_eq!(error.code(), crate::ImportErrorCode::ValidationFailed);
    }
}
