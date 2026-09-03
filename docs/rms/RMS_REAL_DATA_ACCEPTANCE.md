# RMS real-data acceptance

## Purpose and evidence boundary

This document records the real robot and flight-telemetry acceptance evidence collected on 2026-08-22.
It distinguishes checked-in automated evidence from external fixtures and manual observations.
Only NVIDIA's reviewed `r2b_galileo.mcap` excerpt is stored in the repository.
The ROBOTIS, Zenodo, and DroneKit files remain external to the repository.
An API response marked `ready` proves that this RMS build imported and exposed the recording, but it does not by itself prove production storage durability, device authenticity, or permission to redistribute the source data.

## Acceptance summary

- NVIDIA `r2b_galileo.mcap` is the tracked, non-ignored server regression fixture.
- ROBOTIS episode 103 is an external-only manual Web Viewer and live API acceptance fixture with no redistribution permission established.
- Zenodo record 19469198 is the external, license-declared service-robot acceptance fixture used for a live import and byte-range Replay check.
- DroneKit `flight.tlog` is the external opt-in Edge acceptance fixture used to exercise passive MAVLink heartbeat observation.
- ROS 2 DDS graph discovery, recorded MCAP payload import, and MAVLink heartbeat observation are separate contracts and must not be presented as one interchangeable live-data path.

## FigJam architecture diagram workflow

The crate-organization diagram workflow is defined by the comment next to the image in [`ARCHITECTURE.md`](../../ARCHITECTURE.md).
The source diagram is the [Rerun Crates organization FigJam document](https://www.figma.com/file/Umob8ztK1HmYKLUMSq8aPb/Crates-org).
The diagram is a manually exported image and is not generated from Cargo metadata or refreshed by a build.
It must be updated whenever a crate is added, removed, or updated.

1. Update the FigJam document.
2. Select all content, right-click, and choose **Copy as PNG**.
3. Run `pixi run upload-image --name architecture_diagram`.
4. Replace the existing image HTML in `ARCHITECTURE.md` with the HTML printed by the upload command.

The Markdown crate tables and the FigJam image therefore require separate updates.
A stale FigJam export does not change runtime behavior, but it is an architecture-documentation defect that should be resolved in the same change that alters the crate organization.

## NVIDIA r2b tracked server acceptance

### Provenance and redistribution

The repository fixture [`tests/assets/mcap/r2b_galileo.mcap`](../../tests/assets/mcap/r2b_galileo.mcap) is a modified excerpt of NVIDIA's [r2b Dataset 2024](https://catalog.ngc.nvidia.com/orgs/nvidia/isaac/resources/r2bdataset2024/1).
NVIDIA describes the source as live-recorded, time-synchronized robot sensor captures stored in ROS 2 bags.
The source dataset is licensed under [Creative Commons Attribution 4.0](https://creativecommons.org/licenses/by/4.0/).
The repository's attribution and modification notice are recorded in [`tests/assets/mcap/README.md`](../../tests/assets/mcap/README.md).
The tracked excerpt is 19,001,054 bytes and has SHA-256 `6c65717c9e45cdcb397f8bd40055afcd6b761dee838724a2052c91ca60611cf5`.
It contains 339 messages across 15 ROS 2 channels over approximately 0.669 seconds.
Representative channels include compressed stereo camera images, camera calibration, `/front_stereo_imu/imu`, `/chassis/imu`, and `/chassis/battery_state`.

### Automated evidence

The non-ignored test `physical_robot_mcap_imports_to_ready_recording_and_range_replay` is defined in [`crates/store/rms_server/tests/imports.rs`](../../crates/store/rms_server/tests/imports.rs).
The test reads the checked-in bytes at runtime and verifies the reviewed source SHA-256.
It uploads the MCAP through the public recording-import route and waits for a `ready` Recording.
It verifies materialized physical sensor topics, a non-empty RRD version, footer verification, lossless timestamp bounds, a nonzero duration, and a manifest bound to the converted content hash.
It then creates a Replay session and requires an HTTP `206 Partial Content` response for bytes `0-15` with an `RRF2` prefix.
This test runs in the ordinary server test suite and does not require a network download or opt-in environment variable.

Run the focused acceptance test from the workspace root:

```powershell
cargo nextest run --all-features --no-fail-fast -p rms_server --test imports physical_robot_mcap_imports_to_ready_recording_and_range_replay
```

Inspect the reviewed fixture independently:

```powershell
Get-FileHash -LiteralPath .\tests\assets\mcap\r2b_galileo.mcap -Algorithm SHA256
& .\target\debug\rerun.exe mcap info .\tests\assets\mcap\r2b_galileo.mcap
& .\target\debug\rerun.exe mcap check .\tests\assets\mcap\r2b_galileo.mcap
```

The full `rms_server` nextest run for this acceptance pass completed 69 non-ignored tests successfully and left one explicitly ignored test unexecuted.

## ROBOTIS external-only visual and API acceptance

### Provenance and license boundary

The external source is the RobotisAI publisher's [evButtonPush-260615-0-MCAP dataset](https://huggingface.co/datasets/RobotisAI/evButtonPush-260615-0-MCAP).
The reviewed file is [episode `103/103_0.mcap`](https://huggingface.co/datasets/RobotisAI/evButtonPush-260615-0-MCAP/blob/main/103/103_0.mcap).
The publisher describes episode 103 as an elevator-button-push task recorded by a `frontier_omy_f3m` robot.
The file is 70,634,881 bytes and has SHA-256 `1f308c38901451169ae6d3d9063d0ead7fffc677e04e4ec610d926efc1c4c88f`.
The ROS 2 Jazzy MCAP contains `/tf`, leader and robot joint states, tactile GPIO state, and three compressed camera streams.

The dataset page and repository metadata do not declare a license.
Public download access is not evidence of a redistribution grant.
Do not commit, bundle, mirror, or redistribute this file unless ROBOTIS supplies terms that authorize the intended use.
Use it only as an external temporary fixture under the operator's own authorization.

### Observed acceptance evidence

The live API import was `recording-import-32f6864d-0912-4136-bd7c-4a912495b30f`.
The API reported `ready`, 100 percent progress, the reviewed source size and SHA-256, and Recording `recording-1120c184-df39-4c29-85ec-e682943180b4`.
The Recording reported RRD version `0.36.1`, `footerVerified: true`, content SHA-256 `4411d0ff4202f361d0a511ce8ee2a4536fac03fb8708eacf4c210f5a04bd47b1`, and a maximum preserved timeline duration of approximately 10.970 seconds.
The integrated RMS Replay screen opened the converted recording in the embedded Web Viewer without an iframe.
The camera data, robot topic inventory, timestamp timeline, and playback controls were manually inspected.
This visual check is a manual acceptance observation and is not a checked-in screenshot regression.

Download and verify the external file in a new temporary directory:

```powershell
$robotisAudit = Join-Path ([System.IO.Path]::GetTempPath()) ("rms-robotis-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $robotisAudit | Out-Null
$robotisMcap = Join-Path $robotisAudit "robotis-episode-103.mcap"
curl.exe --fail --location --output $robotisMcap "https://huggingface.co/datasets/RobotisAI/evButtonPush-260615-0-MCAP/resolve/main/103/103_0.mcap?download=true"
Get-FileHash -LiteralPath $robotisMcap -Algorithm SHA256
& .\target\debug\rerun.exe mcap info $robotisMcap
& .\target\debug\rerun.exe mcap check $robotisMcap
```

Query the already observed development import and Recording:

```powershell
$serverBase = "http://127.0.0.1:8080"
curl.exe --fail-with-body "$serverBase/api/v1/recording-imports/recording-import-32f6864d-0912-4136-bd7c-4a912495b30f"
curl.exe --fail-with-body --dump-header - --range 0-31 --output NUL "$serverBase/rerun/recordings/recording-1120c184-df39-4c29-85ec-e682943180b4"
```

These fixed development identifiers are evidence from this run and are not portable identifiers for a fresh server state.

## Zenodo service-robot live API acceptance

### Provenance and redistribution

The external source is [Zenodo record 19469198](https://zenodo.org/records/19469198), DOI [`10.5281/zenodo.19469198`](https://doi.org/10.5281/zenodo.19469198).
The authors describe data collected from a ROS 2 service robot executing the RoboCup@Home Robot Inspection task.
Test 3 applies a denial-of-service attack to the LiDAR path and records degraded navigation and a collision.
The record declares the dataset as `GPL-3.0-or-later`.
Redistributed copies or derived fixtures must preserve the applicable [GNU GPL version 3 terms](https://www.gnu.org/licenses/gpl-3.0.html), source attribution, and notices.
The selected [Test_3-E3 archive](https://zenodo.org/api/records/19469198/files/Test_3-E3.zip/content) is 154,889,239 bytes with Zenodo MD5 `51a28d76652ec42233f58852d679038b`.
The downloaded archive had SHA-256 `94e9b0c78d52481387b82d03178ac8d3d55407511fc0c822d96e2e125e145bac`.
Its extracted MCAP is 266,728,375 bytes with SHA-256 `b4cdfec1aaaa1f477a43b9146b63647ae737c52550f32d2a7aac057574cdf5fe`.
Rerun's MCAP checker accepted all ten data channels and reported 3,111 complete messages over approximately 12.268 seconds of message-log time.

### Live API evidence

The live import was `recording-import-15be73ee-66cb-454a-9a7b-756754ff8459` and reached `ready` at 100 percent.
The import preserved the 266,728,375-byte source and its reviewed SHA-256.
The resulting Recording was `recording-16e1fd43-41c1-4a81-87a7-08c4fa8f68c4`.
The Recording reported RRD version `0.36.1`, `footerVerified: true`, content SHA-256 `8ea333a295d28cec596fa2452a737f3ec2d8f3c006479c5082497b580cbceaec`, and a maximum timeline duration of `40.560992792` seconds.
The workspace materialized 12 entries including MCAP metadata plus `/direct_laser_odometry/odom`, `/head_front_camera/rgb/image_raw`, `/joy_priority`, `/local_costmap/costmap`, `/map`, `/power/is_emergency`, `/rosout`, `/scan_raw`, `/tf_drop`, and `/tf_static`.
Replay session `replay-session-71338b60-ab22-4094-a42c-7707b1f4122e` returned `206 Partial Content` for bytes `0-31`.
The response reported `Content-Range: bytes 0-31/201042392`, advertised byte ranges, and began with the `RRF2` file signature.
The import also reported that crash-durable directory synchronization was unavailable on this development worker.
The acceptance result therefore covers format validation, conversion, catalog materialization, and Replay serving, but it does not waive the production storage-durability requirement described in the [server import contract](../../crates/store/rms_server/README.md#recording-imports).

### Reproducible import and Replay commands

Create a new temporary audit directory, download the official archive, and verify its contents:

```powershell
$zenodoAudit = Join-Path ([System.IO.Path]::GetTempPath()) ("rms-zenodo-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $zenodoAudit | Out-Null
$zenodoZip = Join-Path $zenodoAudit "Test_3-E3.zip"
curl.exe --fail --location --output $zenodoZip "https://zenodo.org/api/records/19469198/files/Test_3-E3.zip/content"
Get-FileHash -LiteralPath $zenodoZip -Algorithm SHA256
$zenodoExtracted = Join-Path $zenodoAudit "extracted"
Expand-Archive -LiteralPath $zenodoZip -DestinationPath $zenodoExtracted
$zenodoMcap = Join-Path $zenodoExtracted "Test_3-E3\rosbag_24-Mar-10_21\rosbag_24-Mar-10_21_0.mcap"
Get-FileHash -LiteralPath $zenodoMcap -Algorithm SHA256
& .\target\debug\rerun.exe mcap info $zenodoMcap
& .\target\debug\rerun.exe mcap check $zenodoMcap
```

Upload the MCAP through the ordered multipart API and wait for a terminal state:

```powershell
$serverBase = "http://127.0.0.1:8080"
$importJson = curl.exe --fail-with-body -H "Idempotency-Key: zenodo-19469198-test-3-e3-v1" -F "projectId=project-logistics" -F "deviceId=robot-07" -F "format=mcap" -F "file=@$zenodoMcap" "$serverBase/api/v1/recording-imports"
$import = $importJson | ConvertFrom-Json
do {
    Start-Sleep -Seconds 1
    $ready = (curl.exe --fail-with-body "$serverBase/api/v1/recording-imports/$($import.id)") | ConvertFrom-Json
} until ($ready.status -in @("ready", "failed"))
if ($ready.status -ne "ready") {
    throw "MCAP import failed: $($ready.failureReason)"
}
$ready | ConvertTo-Json -Depth 8
```

Create a Replay session and verify a byte-range response without downloading the complete RRD:

```powershell
$replayRequest = @{ projectId = "project-logistics"; recordingId = $ready.recordingId; openedBy = "real-data-acceptance" } | ConvertTo-Json -Compress
$replay = (curl.exe --fail-with-body -H "Content-Type: application/json" --data-binary $replayRequest "$serverBase/api/v1/replay-sessions") | ConvertFrom-Json
$replayPrefix = Join-Path $zenodoAudit "replay-prefix.bin"
curl.exe --fail-with-body --dump-header - --range 0-31 --output $replayPrefix "$serverBase$($replay.streamUrl)"
$prefixBytes = [System.IO.File]::ReadAllBytes($replayPrefix)
[System.Text.Encoding]::ASCII.GetString($prefixBytes, 0, 4)
```

The final command must print `RRF2`, and the response headers must report `206 Partial Content` and a matching `Content-Range`.

## DroneKit MAVLink opt-in Edge acceptance

### Provenance and redistribution

The external fixture is [`examples/flight_replay/flight.tlog`](https://github.com/dronekit/dronekit-python/blob/master/examples/flight_replay/flight.tlog) from the official [DroneKit Python repository](https://github.com/dronekit/dronekit-python).
The accompanying [`flight_replay.py`](https://github.com/dronekit/dronekit-python/blob/master/examples/flight_replay/flight_replay.py) describes replaying a past DroneShare flight, and the [fixture-introduction commit](https://github.com/dronekit/dronekit-python/commit/17bb5a4652a28458d40688a5c2759c823b03f4e8) records the move to a local telemetry log.
DroneKit Python is distributed under the repository's [Apache License 2.0](https://github.com/dronekit/dronekit-python/blob/master/LICENSE).
Redistribution under that license requires preservation of the applicable copyright, license, notice, and modification terms.
The reviewed file is 2,723,840 bytes with SHA-256 `434c0492f77c3afe99bcfe05c03bd09a65e2125cb0c4407ba9e32577b161adce`.
It contains 76,791 complete MAVLink v1 records, 1,047 system-1 HEARTBEAT frames, and 424 system-1 HEARTBEAT frames with the armed bit set.
The capture also ends with one 19-byte incomplete record, which the bounded test parser recognizes only as an incomplete final tail and never attempts to resynchronize through.

The first-party source establishes that this is recorded flight telemetry, but it does not identify whether the producing vehicle was physical hardware or SITL.
GPS movement and armed states are not sufficient to prove hardware provenance because a simulator can emit the same fields.
The acceptance claim is therefore limited to a real external DroneKit telemetry capture and must not be worded as confirmed physical-aircraft evidence.

### Opt-in automated evidence

The ignored test is [`crates/top/rms_edge_agent/tests/real_mavlink_tlog.rs`](../../crates/top/rms_edge_agent/tests/real_mavlink_tlog.rs).
The test requires `RMS_REAL_MAVLINK_TLOG` to be an absolute path and does not download or alter the fixture.
It caps input at 16 MiB and 1,000,000 records, validates the reviewed size and SHA-256, parses only an eight-byte big-endian timestamp followed by a complete MAVLink v1 frame, and never invokes a shell or configurable subprocess.
It removes only the timestamp prefix and sends one original armed system-1 HEARTBEAT frame to a localhost UDP socket.
It constructs the public `MavlinkAdapter::from_config`, polls through the public `Adapter` contract, and requires exactly one observation with identity `mavlink:1`, component aggregation metadata, the armed base-mode bit, observed trust, and `metadata_only` source status.
The actual external-fixture run completed one test successfully.

Download the fixture into a new temporary directory and verify it:

```powershell
$mavlinkAudit = Join-Path ([System.IO.Path]::GetTempPath()) ("rms-mavlink-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $mavlinkAudit | Out-Null
$mavlinkTlog = Join-Path $mavlinkAudit "flight.tlog"
curl.exe --fail --location --output $mavlinkTlog "https://raw.githubusercontent.com/dronekit/dronekit-python/master/examples/flight_replay/flight.tlog"
Get-FileHash -LiteralPath $mavlinkTlog -Algorithm SHA256
```

Run only the opt-in real-data test:

```powershell
$env:RMS_REAL_MAVLINK_TLOG = (Resolve-Path -LiteralPath $mavlinkTlog).Path
cargo nextest run --all-features --no-fail-fast -p rms_edge_agent --test real_mavlink_tlog --run-ignored ignored-only
```

An ordinary `rms_edge_agent` test run compiles this test but skips its external fixture dependency.

## Operational scope limits

The ROS 2 DDS Edge adapter is an observation-only graph inventory service as specified in [`RMS_ROS2_EDGE_ADAPTER.md`](RMS_ROS2_EDGE_ADAPTER.md).
It reports bounded node, topic, type, association, and QoS metadata and marks all discovered ROS sources as `metadata_only`.
It does not subscribe to application samples, publish topics, call services, invoke actions, or capture an MCAP payload.

The RMS server MCAP importer is a separate recorded-data path.
It accepts uploaded MCAP payload bytes, verifies and converts supported content into RRD, materializes topic metadata, and serves immutable byte-range Replay streams.
Successful MCAP Replay does not imply that the ROS 2 Edge adapter can stream those payloads live.

The MAVLink Edge adapter is passive and currently consumes valid HEARTBEAT metadata only.
It does not import `.tlog` files directly, expose GPS or arbitrary MAVLink payload streams, send commands, arm a vehicle, change flight mode, or control actuators.
The opt-in test's UDP sender is test-only replay into localhost and is not a device-control path.

None of these acceptance checks grants a physical control lease or demonstrates physical robot, vehicle, or aircraft control.
A production control plane requires a separate authenticated and authorized command contract with device-specific safety enforcement, and that contract is outside this evidence set.
