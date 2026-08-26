use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::{ImportCancellation, ImportError, ImportErrorCode};

const TOOL_TIMEOUT: Duration = Duration::from_mins(10);
const MAX_TOOL_DIAGNOSTIC_BYTES: usize = 16 * 1024;
const MAX_TOOL_DIAGNOSTIC_DISK_BYTES: u64 = 16 * 1024 * 1024;
const TOOL_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) fn transcode_to_mp4(
    source: &Path,
    output: &Path,
    max_bytes: u64,
    cancellation: Option<&ImportCancellation>,
) -> Result<(), ImportError> {
    let ffmpeg = locate_tool("ffmpeg", &ffmpeg_candidates(), "-version").ok_or_else(|| {
        ImportError::new(
            ImportErrorCode::ToolUnavailable,
            "MOV and WebM imports require ffmpeg on the import worker.",
            "ffmpeg was not found on PATH or in the RMS managed installation path",
        )
    })?;

    let args = [
        OsString::from("-nostdin"),
        OsString::from("-hide_banner"),
        OsString::from("-loglevel"),
        OsString::from("error"),
        OsString::from("-y"),
        OsString::from("-i"),
        source.as_os_str().to_owned(),
        OsString::from("-map"),
        OsString::from("0:v:0"),
        OsString::from("-an"),
        OsString::from("-c:v"),
        OsString::from("libx264"),
        OsString::from("-pix_fmt"),
        OsString::from("yuv420p"),
        OsString::from("-bf"),
        OsString::from("0"),
        OsString::from("-movflags"),
        OsString::from("+faststart"),
        OsString::from("-fs"),
        OsString::from(max_bytes.to_string()),
        output.as_os_str().to_owned(),
    ];
    run_tool(
        &ffmpeg,
        &args,
        TOOL_TIMEOUT,
        "The video could not be transcoded to a Replay-compatible MP4.",
        cancellation,
        Some((output, max_bytes)),
        output.parent(),
    )
}

pub(crate) fn convert_rosbag_to_mcap(
    source: &Path,
    output_dir: &Path,
    max_bytes: u64,
    cancellation: Option<&ImportCancellation>,
) -> Result<(), ImportError> {
    let rosbags =
        locate_tool("rosbags-convert", &rosbags_candidates(), "--help").ok_or_else(|| {
            ImportError::new(
                ImportErrorCode::ToolUnavailable,
                "SQLite ROS 2 bags require rosbags-convert 0.11.3 on the import worker.",
                "rosbags-convert was not found on PATH; install the pinned rosbags 0.11.3 adapter",
            )
        })?;

    let args = [
        OsString::from("--src"),
        source.as_os_str().to_owned(),
        OsString::from("--dst"),
        output_dir.as_os_str().to_owned(),
        OsString::from("--dst-storage"),
        OsString::from("mcap"),
        OsString::from("--dst-version"),
        OsString::from("9"),
    ];
    run_tool(
        &rosbags,
        &args,
        TOOL_TIMEOUT,
        "The SQLite ROS 2 bag could not be converted to MCAP.",
        cancellation,
        Some((output_dir, max_bytes)),
        output_dir.parent(),
    )
}

#[cfg(test)]
pub(crate) fn transcode_fixture_to_webm(source: &Path, output: &Path) -> Result<(), ImportError> {
    let ffmpeg = locate_tool("ffmpeg", &ffmpeg_candidates(), "-version").ok_or_else(|| {
        ImportError::new(
            ImportErrorCode::ToolUnavailable,
            "The ffmpeg test dependency is unavailable.",
            "ffmpeg was not detected for the WebM conversion test",
        )
    })?;
    let args = [
        OsString::from("-nostdin"),
        OsString::from("-hide_banner"),
        OsString::from("-loglevel"),
        OsString::from("error"),
        OsString::from("-y"),
        OsString::from("-i"),
        source.as_os_str().to_owned(),
        OsString::from("-t"),
        OsString::from("0.5"),
        OsString::from("-vf"),
        OsString::from("scale=320:-2"),
        OsString::from("-an"),
        OsString::from("-c:v"),
        OsString::from("libvpx-vp9"),
        OsString::from("-deadline"),
        OsString::from("realtime"),
        OsString::from("-cpu-used"),
        OsString::from("8"),
        output.as_os_str().to_owned(),
    ];
    run_tool(
        &ffmpeg,
        &args,
        Duration::from_mins(2),
        "The test fixture could not be transcoded to WebM.",
        None,
        Some((output, 32 * 1024 * 1024)),
        output.parent(),
    )
}

#[cfg(test)]
pub(crate) fn convert_fixture_mcap_to_db3(
    source: &Path,
    output_dir: &Path,
) -> Result<(), ImportError> {
    let rosbags =
        locate_tool("rosbags-convert", &rosbags_candidates(), "--help").ok_or_else(|| {
            ImportError::new(
                ImportErrorCode::ToolUnavailable,
                "The rosbags-convert test dependency is unavailable.",
                "rosbags-convert was not detected for the sqlite3 conversion test",
            )
        })?;
    let args = [
        OsString::from("--src"),
        source.as_os_str().to_owned(),
        OsString::from("--dst"),
        output_dir.as_os_str().to_owned(),
        OsString::from("--dst-storage"),
        OsString::from("sqlite3"),
        OsString::from("--dst-version"),
        OsString::from("9"),
    ];
    run_tool(
        &rosbags,
        &args,
        Duration::from_mins(2),
        "The test MCAP could not be converted to sqlite3.",
        None,
        Some((output_dir, 64 * 1024 * 1024)),
        output_dir.parent(),
    )
}

fn ffmpeg_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("RMS_FFMPEG") {
        candidates.push(PathBuf::from(path));
    }
    #[cfg(windows)]
    {
        candidates.push(PathBuf::from("ffmpeg.exe"));
        candidates.push(PathBuf::from(r"C:\FFmpeg\ffmpeg.exe"));
    }
    #[cfg(not(windows))]
    candidates.push(PathBuf::from("ffmpeg"));
    candidates
}

fn rosbags_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("RMS_ROSBAGS_CONVERT") {
        candidates.push(PathBuf::from(path));
    }
    #[cfg(windows)]
    {
        if let Some(user_profile) = std::env::var_os("USERPROFILE") {
            candidates.push(
                PathBuf::from(user_profile)
                    .join(".local")
                    .join("bin")
                    .join("rosbags-convert.exe"),
            );
        }
        if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
            let python_root = PathBuf::from(local_app_data)
                .join("Programs")
                .join("Python");
            if let Ok(entries) = std::fs::read_dir(python_root) {
                let mut discovered = entries
                    .flatten()
                    .map(|entry| entry.path().join("Scripts").join("rosbags-convert.exe"))
                    .collect::<Vec<_>>();
                discovered.sort();
                candidates.extend(discovered);
            }
        }
        candidates.push(PathBuf::from("rosbags-convert.exe"));
    }
    #[cfg(not(windows))]
    candidates.push(PathBuf::from("rosbags-convert"));
    candidates
}

fn locate_tool(display_name: &str, candidates: &[PathBuf], version_arg: &str) -> Option<PathBuf> {
    candidates.iter().find_map(|candidate| {
        if probe_tool(candidate, version_arg) {
            Some(candidate.clone())
        } else {
            re_log::debug!(
                tool = display_name,
                candidate = %candidate.display(),
                "Importer tool probe returned a failure status"
            );
            None
        }
    })
}

fn probe_tool(executable: &Path, argument: &str) -> bool {
    let mut command = Command::new(executable);
    command
        .arg(argument)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    apply_restricted_environment(&mut command);
    configure_process_group(&mut command);
    let Ok(mut child) = command.spawn() else {
        return false;
    };
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if started.elapsed() < TOOL_PROBE_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) | Err(_) => {
                terminate_process_tree(&mut child);
                return false;
            }
        }
    }
}

fn run_tool(
    executable: &Path,
    args: &[impl AsRef<OsStr>],
    timeout: Duration,
    user_reason: &'static str,
    cancellation: Option<&ImportCancellation>,
    monitored_output: Option<(&Path, u64)>,
    current_directory: Option<&Path>,
) -> Result<(), ImportError> {
    let diagnostic = tempfile::NamedTempFile::new().map_err(|err| {
        ImportError::new(
            ImportErrorCode::Io,
            "The import worker could not create a diagnostic file.",
            format!("Failed to create tool diagnostic file: {err}"),
        )
    })?;
    let stderr = diagnostic.reopen().map_err(|err| {
        ImportError::new(
            ImportErrorCode::Io,
            "The import worker could not create a diagnostic file.",
            format!("Failed to reopen tool diagnostic file: {err}"),
        )
    })?;
    let mut command = Command::new(executable);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr));
    apply_restricted_environment(&mut command);
    if let Some(current_directory) = current_directory {
        command.current_dir(current_directory);
    }
    configure_process_group(&mut command);
    let mut child = command.spawn().map_err(|err| {
        ImportError::new(
            ImportErrorCode::ToolUnavailable,
            "A required import tool could not be started.",
            format!("Failed to start {}: {err}", executable.display()),
        )
    })?;

    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|err| {
            ImportError::new(
                ImportErrorCode::ConversionFailed,
                user_reason,
                format!("Failed while waiting for {}: {err}", executable.display()),
            )
        })? {
            break status;
        }
        if cancellation.is_some_and(ImportCancellation::is_cancelled) {
            terminate_process_tree(&mut child);
            return Err(ImportError::cancelled());
        }
        match diagnostic.as_file().metadata() {
            Ok(metadata) if metadata.len() > MAX_TOOL_DIAGNOSTIC_DISK_BYTES => {
                terminate_process_tree(&mut child);
                return Err(ImportError::new(
                    ImportErrorCode::ResourceLimitExceeded,
                    "The import tool exceeded its diagnostic output limit.",
                    format!(
                        "Tool diagnostic output exceeded {MAX_TOOL_DIAGNOSTIC_DISK_BYTES} bytes: {}",
                        executable.display()
                    ),
                ));
            }
            Ok(_) => {}
            Err(err) => {
                terminate_process_tree(&mut child);
                return Err(ImportError::new(
                    ImportErrorCode::ConversionFailed,
                    user_reason,
                    format!(
                        "Failed to monitor tool diagnostic output: {err}\nFile path: {}",
                        diagnostic.path().display()
                    ),
                ));
            }
        }
        if let Some((path, max_bytes)) = monitored_output {
            let generated_bytes = match generated_size(path) {
                Ok(generated_bytes) => generated_bytes,
                Err(err) => {
                    terminate_process_tree(&mut child);
                    return Err(err);
                }
            };
            if generated_bytes > max_bytes {
                terminate_process_tree(&mut child);
                return Err(ImportError::new(
                    ImportErrorCode::ResourceLimitExceeded,
                    "The import tool exceeded its temporary output size limit.",
                    format!(
                        "Tool generated {generated_bytes} bytes; limit is {max_bytes}\nPath: {}",
                        path.display()
                    ),
                ));
            }
        }
        if started.elapsed() >= timeout {
            terminate_process_tree(&mut child);
            return Err(ImportError::new(
                ImportErrorCode::ResourceLimitExceeded,
                "The import tool exceeded its 10 minute execution limit.",
                format!("Timed out while running {}", executable.display()),
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    };

    let diagnostic_size = diagnostic
        .as_file()
        .metadata()
        .map_err(|err| {
            ImportError::new(
                ImportErrorCode::ConversionFailed,
                user_reason,
                format!(
                    "Failed to inspect completed tool diagnostic output: {err}\nFile path: {}",
                    diagnostic.path().display()
                ),
            )
        })?
        .len();
    if diagnostic_size > MAX_TOOL_DIAGNOSTIC_DISK_BYTES {
        return Err(ImportError::new(
            ImportErrorCode::ResourceLimitExceeded,
            "The import tool exceeded its diagnostic output limit.",
            format!(
                "Tool diagnostic output is {diagnostic_size} bytes; maximum is {MAX_TOOL_DIAGNOSTIC_DISK_BYTES}"
            ),
        ));
    }
    if let Some((path, max_bytes)) = monitored_output {
        let generated_bytes = generated_size(path)?;
        if generated_bytes > max_bytes {
            return Err(ImportError::new(
                ImportErrorCode::ResourceLimitExceeded,
                "The import tool exceeded its temporary output size limit.",
                format!(
                    "Tool generated {generated_bytes} bytes; limit is {max_bytes}\nPath: {}",
                    path.display()
                ),
            ));
        }
    }
    if status.success() {
        return Ok(());
    }
    let mut detail = std::fs::read(diagnostic.path()).unwrap_or_default();
    if detail.len() > MAX_TOOL_DIAGNOSTIC_BYTES {
        detail = detail.split_off(detail.len() - MAX_TOOL_DIAGNOSTIC_BYTES);
    }
    let detail = String::from_utf8_lossy(&detail);
    Err(ImportError::new(
        ImportErrorCode::ConversionFailed,
        user_reason,
        format!(
            "{} exited with {status}; diagnostic tail:\n{detail}",
            executable.display()
        ),
    ))
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt as _;
    command.process_group(0);
}

#[cfg(not(unix))]
fn configure_process_group(_command: &mut Command) {}

fn apply_restricted_environment(command: &mut Command) {
    const ALLOWED_ENVIRONMENT: &[&str] = &[
        "SystemRoot",
        "WINDIR",
        "ComSpec",
        "PATH",
        "PATHEXT",
        "TEMP",
        "TMP",
        "TMPDIR",
        "HOME",
        "USERPROFILE",
        "LANG",
        "LC_ALL",
    ];
    let allowed = ALLOWED_ENVIRONMENT
        .iter()
        .filter_map(|name| std::env::var_os(name).map(|value| (*name, value)))
        .collect::<Vec<_>>();
    command.env_clear().envs(allowed);
}

fn generated_size(path: &Path) -> Result<u64, ImportError> {
    if !path.exists() {
        return Ok(0);
    }
    let mut entry_count = 0_usize;
    generated_size_inner(path, 0, &mut entry_count)
}

fn generated_size_inner(
    path: &Path,
    depth: usize,
    entry_count: &mut usize,
) -> Result<u64, ImportError> {
    if depth > 8 || *entry_count > 4096 {
        return Err(ImportError::new(
            ImportErrorCode::ResourceLimitExceeded,
            "The import tool created an unexpectedly deep or wide output tree.",
            format!("Tool output tree limit exceeded\nPath: {}", path.display()),
        ));
    }
    *entry_count += 1;
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            // SQLite and similar tools may remove short-lived journal files between `read_dir`
            // and this metadata lookup. They no longer consume the monitored output budget.
            return Ok(0);
        }
        Err(err) => {
            return Err(ImportError::new(
                ImportErrorCode::ConversionFailed,
                "The import tool output could not be inspected.",
                format!(
                    "Failed to inspect tool output: {err}\nPath: {}",
                    path.display()
                ),
            ));
        }
    };
    if metadata.file_type().is_symlink() {
        return Err(ImportError::new(
            ImportErrorCode::ValidationFailed,
            "The import tool produced a symbolic link.",
            format!("Tool output symlink rejected\nPath: {}", path.display()),
        ));
    }
    if metadata.is_file() {
        return Ok(metadata.len());
    }
    if !metadata.is_dir() {
        return Err(ImportError::new(
            ImportErrorCode::ValidationFailed,
            "The import tool produced an unsupported filesystem object.",
            format!("Unsupported tool output object\nPath: {}", path.display()),
        ));
    }
    let mut total = 0_u64;
    for entry in std::fs::read_dir(path).map_err(|err| {
        ImportError::new(
            ImportErrorCode::ConversionFailed,
            "The import tool output could not be inspected.",
            format!(
                "Failed to enumerate tool output: {err}\nPath: {}",
                path.display()
            ),
        )
    })? {
        let entry = entry.map_err(|err| {
            ImportError::new(
                ImportErrorCode::ConversionFailed,
                "The import tool output could not be inspected.",
                format!(
                    "Failed to enumerate tool output: {err}\nPath: {}",
                    path.display()
                ),
            )
        })?;
        total = total
            .checked_add(generated_size_inner(&entry.path(), depth + 1, entry_count)?)
            .ok_or_else(|| {
                ImportError::new(
                    ImportErrorCode::ResourceLimitExceeded,
                    "The import tool output size overflowed its accounting limit.",
                    format!("Tool output size overflow\nPath: {}", path.display()),
                )
            })?;
    }
    Ok(total)
}

fn terminate_process_tree(child: &mut Child) {
    let pid = child.id();
    #[cfg(windows)]
    {
        drop(
            Command::new("taskkill.exe")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status(),
        );
    }
    #[cfg(unix)]
    {
        drop(
            Command::new("kill")
                .args(["-KILL", &format!("-{pid}")])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status(),
        );
    }
    drop(child.kill());
    drop(child.wait());
}

#[cfg(test)]
mod tests {
    use std::ffi::{OsStr, OsString};

    use super::run_tool;

    #[test]
    fn tool_arguments_are_passed_without_a_shell() {
        let executable = std::env::var_os("RUSTC").unwrap_or_else(|| OsString::from("rustc"));
        let args: Vec<&OsStr> = vec![OsStr::new("--version")];
        run_tool(
            std::path::Path::new(&executable),
            &args,
            std::time::Duration::from_secs(5),
            "tool failed",
            None,
            None,
            None,
        )
        .unwrap();
    }
}
