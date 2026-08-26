use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use re_chunk::{Chunk, RowId};
use re_log_types::{EntityPath, TimeCell, TimePoint};
use re_sdk_types::archetypes::{Scalars, SeriesLines};
use serde::Deserialize;

use crate::{ImportError, ImportErrorCode, RecordingWriter};

const MAX_CSV_COLUMNS: usize = 256;
const MAX_CSV_ROWS: u64 = 1_000_000;
const MAX_CSV_RECORD_BYTES: usize = 1024 * 1024;
const MAX_CSV_FIELD_BYTES: usize = 256 * 1024;
const CSV_BATCH_ROWS: usize = 4096;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum CsvTimelineKind {
    Sequence,
    Timestamp,
    Duration,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum CsvTimeUnit {
    #[default]
    Ns,
    Us,
    Ms,
    S,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CsvTimelineMapping {
    column: String,
    #[serde(default = "default_timeline_name")]
    name: String,
    kind: CsvTimelineKind,
    #[serde(default)]
    unit: CsvTimeUnit,
    #[serde(default)]
    fps: Option<f64>,
}

fn default_timeline_name() -> String {
    "time".to_owned()
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CsvColumnMapping {
    column: String,
    entity_path: String,
    #[serde(default)]
    unit: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CsvMapping {
    #[serde(default)]
    timeline: Option<CsvTimelineMapping>,
    #[serde(default)]
    entity_path_prefix: Option<String>,
    #[serde(default)]
    columns: Vec<CsvColumnMapping>,
}

pub(crate) struct CsvImportSummary {
    pub default_timeline: String,
    pub fps_hints: BTreeMap<String, f64>,
    pub topics: Vec<String>,
    pub warnings: Vec<String>,
}

struct CsvPlan {
    timeline_name: re_log_types::TimelineName,
    timeline_kind: CsvTimelineKind,
    timeline_unit: CsvTimeUnit,
    timeline_column: Option<usize>,
    fps: Option<f64>,
    columns: Vec<PlannedColumn>,
    warnings: Vec<String>,
}

struct PlannedColumn {
    source_index: usize,
    source_name: String,
    entity_path: EntityPath,
    unit: Option<String>,
}

pub(crate) fn import_csv(
    source: &Path,
    mapping: Option<&serde_json::Value>,
    writer: &mut RecordingWriter<'_>,
) -> Result<CsvImportSummary, ImportError> {
    validate_csv_source_bounded(source, writer)?;
    let mapping = mapping
        .cloned()
        .map(serde_json::from_value::<CsvMapping>)
        .transpose()
        .map_err(|err| {
            ImportError::new(
                ImportErrorCode::InvalidRequest,
                "The CSV mapping is invalid.",
                format!("Failed to deserialize CSV mapping: {err}"),
            )
        })?
        .unwrap_or_default();
    let plan = prepare_plan(source, mapping, writer)?;
    append_series_metadata(writer, &plan.columns)?;

    let mut reader = csv_reader(source)?;
    let mut rows = Vec::with_capacity(CSV_BATCH_ROWS);
    let mut row_index = 0_i64;
    for record in reader.records() {
        writer.check_cancelled()?;
        let record = record.map_err(|err| csv_validation_error(source, &err))?;
        let timeline = match plan.timeline_column {
            Some(index) => parse_timeline(
                record.get(index).unwrap_or_default(),
                plan.timeline_kind,
                plan.timeline_unit,
            )?,
            None => row_index,
        };
        let values = plan
            .columns
            .iter()
            .map(|column| {
                parse_optional_scalar(record.get(column.source_index).unwrap_or_default())
            })
            .collect::<Result<Vec<_>, _>>()?;
        rows.push((timeline, values));
        row_index = row_index.checked_add(1).ok_or_else(|| {
            ImportError::new(
                ImportErrorCode::ResourceLimitExceeded,
                "The CSV contains too many rows.",
                "CSV row sequence overflowed i64",
            )
        })?;
        if rows.len() == CSV_BATCH_ROWS {
            append_csv_batch(writer, &plan, &rows)?;
            rows.clear();
        }
    }
    if !rows.is_empty() {
        append_csv_batch(writer, &plan, &rows)?;
    }

    let mut fps_hints = BTreeMap::new();
    if let Some(fps) = plan.fps {
        fps_hints.insert(plan.timeline_name.to_string(), fps);
    }
    Ok(CsvImportSummary {
        default_timeline: plan.timeline_name.to_string(),
        fps_hints,
        topics: plan
            .columns
            .iter()
            .map(|column| column.source_name.clone())
            .collect(),
        warnings: plan.warnings,
    })
}

fn validate_csv_source_bounded(
    source: &Path,
    writer: &RecordingWriter<'_>,
) -> Result<(), ImportError> {
    use std::io::Read as _;

    let mut file = std::fs::File::open(source).map_err(|err| {
        ImportError::new(
            ImportErrorCode::Io,
            "The CSV source could not be read.",
            format!(
                "Failed to open CSV for bounded preflight: {err}\nFile path: {}",
                source.display()
            ),
        )
    })?;
    let mut buffer = vec![0_u8; 1024 * 1024];
    let mut record_bytes = 0_usize;
    let mut field_bytes = 0_usize;
    let mut in_quotes = false;
    let mut after_quote = false;
    loop {
        writer.check_cancelled()?;
        let count = file.read(&mut buffer).map_err(|err| {
            ImportError::new(
                ImportErrorCode::Io,
                "The CSV source could not be read.",
                format!(
                    "CSV bounded preflight failed: {err}\nFile path: {}",
                    source.display()
                ),
            )
        })?;
        if count == 0 {
            break;
        }
        for byte in &buffer[..count] {
            record_bytes = record_bytes.saturating_add(1);
            field_bytes = field_bytes.saturating_add(1);
            if record_bytes > MAX_CSV_RECORD_BYTES || field_bytes > MAX_CSV_FIELD_BYTES {
                return Err(ImportError::new(
                    ImportErrorCode::ResourceLimitExceeded,
                    "A CSV row or field exceeds the import size limit.",
                    format!(
                        "Bounded CSV preflight exceeded row limit {MAX_CSV_RECORD_BYTES} or field limit {MAX_CSV_FIELD_BYTES}"
                    ),
                ));
            }

            if in_quotes {
                if *byte == b'"' {
                    in_quotes = false;
                    after_quote = true;
                }
                continue;
            }
            if after_quote {
                if *byte == b'"' {
                    in_quotes = true;
                    after_quote = false;
                    continue;
                }
                after_quote = false;
            } else if *byte == b'"' && field_bytes == 1 {
                in_quotes = true;
                continue;
            }

            match *byte {
                b',' => field_bytes = 0,
                b'\n' => {
                    record_bytes = 0;
                    field_bytes = 0;
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn prepare_plan(
    source: &Path,
    mapping: CsvMapping,
    writer: &RecordingWriter<'_>,
) -> Result<CsvPlan, ImportError> {
    let mut reader = csv_reader(source)?;
    let headers = {
        let headers = reader
            .headers()
            .map_err(|err| csv_validation_error(source, &err))?;
        validate_record_size(headers)?;
        headers.clone()
    };
    if headers.is_empty() || headers.len() > MAX_CSV_COLUMNS {
        return Err(ImportError::new(
            ImportErrorCode::ResourceLimitExceeded,
            "The CSV header has an unsupported number of columns.",
            format!(
                "CSV has {} columns; allowed range is 1..={MAX_CSV_COLUMNS}",
                headers.len()
            ),
        ));
    }
    let mut unique_headers = BTreeSet::new();
    for header in &headers {
        if header.is_empty() || !unique_headers.insert(header.to_owned()) {
            return Err(ImportError::new(
                ImportErrorCode::ValidationFailed,
                "CSV headers must be non-empty and unique.",
                format!("Invalid or duplicate CSV header: {header:?}"),
            ));
        }
    }

    let timeline_column = mapping
        .timeline
        .as_ref()
        .map(|timeline| header_index(&headers, &timeline.column))
        .transpose()?;
    let timeline_name = mapping
        .timeline
        .as_ref()
        .map_or_else(|| "row".to_owned(), |timeline| timeline.name.clone());
    let timeline_name = re_log_types::TimelineName::try_new(&timeline_name).map_err(|err| {
        ImportError::new(
            ImportErrorCode::InvalidRequest,
            "The CSV timeline name is invalid.",
            format!("Invalid CSV timeline name {timeline_name:?}: {err}"),
        )
    })?;
    let timeline_kind = mapping
        .timeline
        .as_ref()
        .map_or(CsvTimelineKind::Sequence, |timeline| timeline.kind);
    let timeline_unit = mapping
        .timeline
        .as_ref()
        .map_or(CsvTimeUnit::Ns, |timeline| timeline.unit);
    let fps = mapping
        .timeline
        .as_ref()
        .and_then(|timeline| timeline.fps)
        .or_else(|| mapping.timeline.is_none().then_some(1.0));
    if let Some(fps) = fps
        && (timeline_kind != CsvTimelineKind::Sequence || !fps.is_finite() || fps <= 0.0)
    {
        return Err(ImportError::new(
            ImportErrorCode::InvalidRequest,
            "CSV FPS must be a positive finite value on a sequence timeline.",
            format!("Invalid CSV timeline FPS: {fps}"),
        ));
    }

    let mut numeric = vec![true; headers.len()];
    let mut seen_numeric = vec![false; headers.len()];
    let explicitly_mapped = mapping
        .columns
        .iter()
        .map(|column| header_index(&headers, &column.column))
        .collect::<Result<Vec<_>, _>>()?;
    let mut unique_mapped_columns = BTreeSet::new();
    for (column, index) in mapping.columns.iter().zip(&explicitly_mapped) {
        if Some(*index) == timeline_column {
            return Err(ImportError::new(
                ImportErrorCode::InvalidRequest,
                "The CSV timeline column cannot also be imported as a value series.",
                format!(
                    "CSV column {:?} is mapped as both timeline and value",
                    column.column
                ),
            ));
        }
        if !unique_mapped_columns.insert(*index) {
            return Err(ImportError::new(
                ImportErrorCode::InvalidRequest,
                "Each CSV source column can only be mapped once.",
                format!("Duplicate mapping for CSV column {:?}", column.column),
            ));
        }
    }
    let mut row_count = 0_u64;
    for record in reader.records() {
        writer.check_cancelled()?;
        let record = record.map_err(|err| csv_validation_error(source, &err))?;
        row_count += 1;
        if row_count > MAX_CSV_ROWS {
            return Err(ImportError::new(
                ImportErrorCode::ResourceLimitExceeded,
                "The CSV exceeds the one million row import limit.",
                format!("CSV row count exceeded {MAX_CSV_ROWS}"),
            ));
        }
        validate_record_size(&record)?;
        if let Some(index) = timeline_column {
            parse_timeline(
                record.get(index).unwrap_or_default(),
                timeline_kind,
                timeline_unit,
            )?;
        }
        for (index, field) in record.iter().enumerate() {
            if Some(index) == timeline_column || field.trim().is_empty() {
                continue;
            }
            match field.trim().parse::<f64>() {
                Ok(value) if value.is_finite() => seen_numeric[index] = true,
                _ => numeric[index] = false,
            }
        }
    }
    if row_count == 0 {
        return Err(ImportError::new(
            ImportErrorCode::ValidationFailed,
            "The CSV contains a header but no data rows.",
            "CSV row count is zero",
        ));
    }

    for index in &explicitly_mapped {
        if !numeric[*index] || !seen_numeric[*index] {
            return Err(ImportError::new(
                ImportErrorCode::ValidationFailed,
                "A mapped CSV value column contains non-numeric or empty data.",
                format!("Mapped column {:?} is not numeric", headers.get(*index)),
            ));
        }
    }

    let prefix = mapping.entity_path_prefix.as_deref().unwrap_or("csv");
    let prefix = EntityPath::parse_strict(prefix).map_err(|err| {
        ImportError::new(
            ImportErrorCode::InvalidRequest,
            "The CSV entity path prefix is invalid.",
            format!("Invalid entityPathPrefix {prefix:?}: {err}"),
        )
    })?;
    let mut warnings = Vec::new();
    if mapping.timeline.is_none() {
        warnings.push("row timeline defaults to 1 FPS".to_owned());
    }
    let columns = if mapping.columns.is_empty() {
        let mut used_paths = BTreeSet::new();
        headers
            .iter()
            .enumerate()
            .filter(|(index, _)| {
                Some(*index) != timeline_column && numeric[*index] && seen_numeric[*index]
            })
            .map(|(index, header)| {
                let mut segment = sanitize_segment(header);
                if !used_paths.insert(segment.clone()) {
                    segment = format!("{segment}_{index}");
                    used_paths.insert(segment.clone());
                }
                PlannedColumn {
                    source_index: index,
                    source_name: header.to_owned(),
                    entity_path: prefix.clone() / EntityPath::parse_forgiving(&segment),
                    unit: None,
                }
            })
            .collect::<Vec<_>>()
    } else {
        mapping
            .columns
            .into_iter()
            .zip(explicitly_mapped)
            .map(|(column, index)| {
                let suffix = EntityPath::parse_strict(&column.entity_path).map_err(|err| {
                    ImportError::new(
                        ImportErrorCode::InvalidRequest,
                        "A CSV entity path is invalid.",
                        format!("Invalid entityPath {:?}: {err}", column.entity_path),
                    )
                })?;
                if column.unit.as_deref().is_some_and(str::is_empty) {
                    warnings.push(format!(
                        "Ignored empty unit for CSV column {}",
                        column.column
                    ));
                }
                Ok(PlannedColumn {
                    source_index: index,
                    source_name: column.column,
                    entity_path: prefix.clone() / suffix,
                    unit: column.unit.filter(|unit| !unit.is_empty()),
                })
            })
            .collect::<Result<Vec<_>, ImportError>>()?
    };
    if columns.is_empty() {
        return Err(ImportError::new(
            ImportErrorCode::ValidationFailed,
            "The CSV has no numeric value columns to visualize.",
            "No numeric CSV columns remained after excluding the timeline",
        ));
    }
    let mut unique_entity_paths = BTreeSet::new();
    for column in &columns {
        let entity_path = column.entity_path.to_string();
        if !unique_entity_paths.insert(entity_path.clone()) {
            return Err(ImportError::new(
                ImportErrorCode::InvalidRequest,
                "Each CSV value series must use a unique entity path.",
                format!("Duplicate mapped CSV entity path: {entity_path}"),
            ));
        }
    }
    for (index, header) in headers.iter().enumerate() {
        if Some(index) != timeline_column && (!numeric[index] || !seen_numeric[index]) {
            warnings.push(format!("Skipped non-numeric CSV column {header}"));
        }
    }

    Ok(CsvPlan {
        timeline_name,
        timeline_kind,
        timeline_unit,
        timeline_column,
        fps,
        columns,
        warnings,
    })
}

fn csv_reader(source: &Path) -> Result<csv::Reader<std::fs::File>, ImportError> {
    csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(false)
        .trim(csv::Trim::All)
        .from_path(source)
        .map_err(|err| csv_validation_error(source, &err))
}

fn csv_validation_error(source: &Path, err: &csv::Error) -> ImportError {
    ImportError::new(
        ImportErrorCode::ValidationFailed,
        "The CSV is not valid UTF-8 tabular data with a consistent row shape.",
        format!("CSV parser failed: {err}\nFile path: {}", source.display()),
    )
}

fn validate_record_size(record: &csv::StringRecord) -> Result<(), ImportError> {
    if record.as_slice().len() > MAX_CSV_RECORD_BYTES
        || record.iter().any(|field| field.len() > MAX_CSV_FIELD_BYTES)
    {
        return Err(ImportError::new(
            ImportErrorCode::ResourceLimitExceeded,
            "A CSV row or field exceeds the import size limit.",
            format!(
                "CSV record size is {} bytes; row limit is {MAX_CSV_RECORD_BYTES} and field limit is {MAX_CSV_FIELD_BYTES}",
                record.as_slice().len()
            ),
        ));
    }
    Ok(())
}

fn header_index(headers: &csv::StringRecord, name: &str) -> Result<usize, ImportError> {
    headers
        .iter()
        .position(|header| header == name)
        .ok_or_else(|| {
            ImportError::new(
                ImportErrorCode::InvalidRequest,
                "The CSV mapping references a column that is not present.",
                format!("CSV column not found: {name:?}"),
            )
        })
}

fn parse_timeline(raw: &str, kind: CsvTimelineKind, unit: CsvTimeUnit) -> Result<i64, ImportError> {
    let raw = raw.trim();
    let value = match kind {
        CsvTimelineKind::Sequence => raw.parse::<i64>().ok(),
        CsvTimelineKind::Timestamp | CsvTimelineKind::Duration => {
            parse_decimal_scaled(raw, unit.scale())
        }
    }
    .filter(|value| *value != i64::MIN)
    .ok_or_else(|| {
        ImportError::new(
            ImportErrorCode::ValidationFailed,
            "A CSV timeline value is invalid or outside the int64 range.",
            format!("Failed to parse CSV timeline value {raw:?} as {kind:?}/{unit:?}"),
        )
    })?;
    Ok(value)
}

impl CsvTimeUnit {
    fn scale(self) -> i64 {
        match self {
            Self::Ns => 1,
            Self::Us => 1_000,
            Self::Ms => 1_000_000,
            Self::S => 1_000_000_000,
        }
    }
}

fn parse_decimal_scaled(raw: &str, scale: i64) -> Option<i64> {
    let (negative, unsigned) = raw
        .strip_prefix('-')
        .map_or((false, raw), |raw| (true, raw));
    let (whole, fractional) = unsigned.split_once('.').unwrap_or((unsigned, ""));
    if whole.is_empty() || !whole.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let whole = whole.parse::<i128>().ok()?;
    let digits = scale.ilog10() as usize;
    let mut fractional = fractional.to_owned();
    if !fractional.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    if fractional.len() > digits {
        if fractional[digits..].bytes().any(|byte| byte != b'0') {
            return None;
        }
        fractional.truncate(digits);
    }
    fractional.extend(std::iter::repeat_n(
        '0',
        digits.saturating_sub(fractional.len()),
    ));
    let fraction = if fractional.is_empty() {
        0_i128
    } else {
        fractional.parse::<i128>().ok()?
    };
    let value = whole
        .checked_mul(i128::from(scale))?
        .checked_add(fraction)?;
    let value = if negative {
        value.checked_neg()?
    } else {
        value
    };
    i64::try_from(value).ok()
}

fn parse_optional_scalar(raw: &str) -> Result<Option<f64>, ImportError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    raw.parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
        .map(Some)
        .ok_or_else(|| {
            ImportError::new(
                ImportErrorCode::ValidationFailed,
                "A mapped CSV value is not a finite number.",
                format!("Failed to parse CSV scalar {raw:?}"),
            )
        })
}

fn append_series_metadata(
    writer: &mut RecordingWriter<'_>,
    columns: &[PlannedColumn],
) -> Result<(), ImportError> {
    for column in columns {
        let display_name = column.unit.as_ref().map_or_else(
            || column.source_name.clone(),
            |unit| format!("{} ({unit})", column.source_name),
        );
        let style = SeriesLines::new().with_names([display_name]);
        let chunk = Chunk::builder(column.entity_path.clone())
            .with_archetype(RowId::new(), TimePoint::STATIC, &style)
            .build()
            .map_err(|err| {
                ImportError::new(
                    ImportErrorCode::ConversionFailed,
                    "The CSV series metadata could not be encoded.",
                    format!("Failed to build CSV SeriesLines chunk: {err}"),
                )
            })?;
        writer.append_chunk(&chunk)?;
    }
    Ok(())
}

fn append_csv_batch(
    writer: &mut RecordingWriter<'_>,
    plan: &CsvPlan,
    rows: &[(i64, Vec<Option<f64>>)],
) -> Result<(), ImportError> {
    for (column_index, column) in plan.columns.iter().enumerate() {
        let mut builder = Chunk::builder(column.entity_path.clone());
        let mut value_count = 0_usize;
        for (time, values) in rows {
            let Some(value) = values[column_index] else {
                continue;
            };
            let cell = match plan.timeline_kind {
                CsvTimelineKind::Sequence => TimeCell::from_sequence(*time),
                CsvTimelineKind::Timestamp => TimeCell::from_timestamp_nanos_since_epoch(*time),
                CsvTimelineKind::Duration => TimeCell::from_duration_nanos(*time),
            };
            let timepoint = TimePoint::default().with_index(plan.timeline_name, cell);
            builder = builder.with_archetype(RowId::new(), timepoint, &Scalars::new([value]));
            value_count += 1;
        }
        if value_count == 0 {
            continue;
        }
        let chunk = builder.build().map_err(|err| {
            ImportError::new(
                ImportErrorCode::ConversionFailed,
                "CSV values could not be encoded into Rerun chunks.",
                format!("Failed to build CSV Scalars chunk: {err}"),
            )
        })?;
        writer.append_chunk(&chunk)?;
    }
    Ok(())
}

fn sanitize_segment(header: &str) -> String {
    let mut result = header
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
    if result.is_empty() {
        result.push_str("column");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::parse_decimal_scaled;

    #[test]
    fn decimal_timestamp_scaling_is_exact() {
        assert_eq!(
            parse_decimal_scaled("1.25", 1_000_000_000),
            Some(1_250_000_000)
        );
        assert_eq!(
            parse_decimal_scaled("-1.25", 1_000_000_000),
            Some(-1_250_000_000)
        );
        assert_eq!(parse_decimal_scaled("1.0000000001", 1_000_000_000), None);
    }
}
