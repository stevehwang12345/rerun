# RMS import boundary

`rms_import` is the blocking, fail-closed conversion boundary used by the RMS upload service.
It converts supported source data into one atomic, footer-backed RRD recording and returns metadata only after the complete RRD payload has been verified.

## Supported formats

| Input | Conversion path | Required mapping or runtime |
| --- | --- | --- |
| RRD | Bounded copy after header, footer, manifest, transport identity, and full chunk decode validation | Exactly one Recording store |
| MCAP | Rerun MCAP importer with raw ROS fallback | Indexed chunk headers must exactly match the embedded summary |
| ROS 2 bag ZIP with MCAP | Safe bounded extraction followed by the MCAP path | `metadata.yaml` must list every segment exactly once |
| ROS 2 bag ZIP with sqlite3 DB3 | Safe bounded extraction, `rosbags-convert`, then the MCAP path | `rosbags-convert` from `rosbags==0.11.3` |
| CSV | Numeric columns become `Scalars` on a typed timeline | Mapping is optional; the default row timeline is 1 FPS |
| MP4 | Rerun MP4 asset importer | ISO-BMFF `ftyp` signature |
| MOV or WebM | Controlled ffmpeg transcode to MP4, then the MP4 path | ffmpeg with H.264 support |

CSV mappings use this optional shape:

```json
{
  "timeline": {
    "column": "elapsed",
    "name": "elapsed",
    "kind": "duration",
    "unit": "s",
    "fps": null
  },
  "entityPathPrefix": "csv",
  "columns": [
    { "column": "temperature", "entityPath": "temperature", "unit": "C" }
  ]
}
```

Timeline kinds are `sequence`, `timestamp`, and `duration`.
Timestamp and duration values are retained as signed int64 nanoseconds and returned as decimal strings.
Without a mapping, the importer skips non-numeric columns and logs each numeric column under `csv/<sanitized-header>`.

## Runtime tools

Set `RMS_FFMPEG` to an explicit ffmpeg executable or make `ffmpeg` available on `PATH`.
Windows workers also probe `C:\FFmpeg\ffmpeg.exe`.
Set `RMS_ROSBAGS_CONVERT` to an explicit `rosbags-convert` executable or make it available on `PATH`.
Windows workers also discover Python installations below `%LOCALAPPDATA%\Programs\Python` without embedding a user-specific path.
The deployment must pin the Python package to `rosbags==0.11.3`.
Setting `RMS_ROSBAGS_CONVERT` to the executable from that pinned environment is the recommended production configuration.

External commands use argument arrays without a shell, a temporary import working directory, cleared environment variables with a small allowlist, null stdin/stdout, bounded stderr, a ten-minute timeout, output-size monitoring, cancellation polling, and whole-process-tree termination.

## Security and operational limits

The importer rejects declared-format and file-signature mismatches before conversion.
ZIP central-directory size and entry count are checked before `ZipArchive` allocation, and extraction rejects traversal, links, duplicate paths, unlisted segments, mixed storage, compression bombs, and request-limit overruns.
RRD validation bounds the footer before allocation, requires one Recording store, validates non-overlapping chunk spans and transport identities, and deep-decodes every chunk in bounded batches.
MCAP validation compares every indexed chunk with its actual raw header before any decompression and applies per-chunk, aggregate, and compression-ratio limits.
Cancellation is cooperative inside hashing, CSV passes, ZIP extraction, chunk conversion, and external-tool polling.
The final RRD is file-synced, atomically renamed without overwrite, and followed by a best-effort parent-directory sync.

ffmpeg and rosbags remain native parsers of untrusted data.
Production deployments must run imports in a separate low-privilege worker or container with network disabled, per-import ACL or mounts, disk quotas, CPU and memory limits, and OS job or cgroup enforcement.
The in-process server integration is suitable for trusted local development, not as the final hostile-upload sandbox.

Call `run_import` from `spawn_blocking` and connect server cancellation to `ImportCancellation`.
