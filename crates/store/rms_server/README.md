# RMS Server

`rms_server` is the local RMS modular-monolith used by the integrated product during development.
It exposes Connect, Projects, Live, Replay, and Live-only Control APIs from one Axum process on `127.0.0.1:8080`.

The binary starts with the deterministic fixture catalog and persists imported recordings in `RMS_STORAGE_DIR`.
The default storage root is `%LOCALAPPDATA%\RMS\storage` on Windows, `$XDG_DATA_HOME/rms` when configured, or the OS temporary-data directory as a fallback.
`fixture_router()` and `AppState::fixture()` deliberately use isolated temporary storage so parallel tests cannot mutate the development registry.
Devices and data sources are organization assets and never contain a project ID.
Projects refer to them through assignments, while every recording owns an immutable project snapshot.

Run the server with:

```powershell
cargo run -p rms_server
```

The fixture Rerun stream endpoints serve `tests/assets/rrd/rms/rms_replay_50s_v0_36_1.rrd`.
It contains a fixed 50-second scenario on a `tick` timeline at 2 Hz and has a verified RRD footer manifest.
Recording metadata exposes the lossless timeline range, default cursor policy, RRD version, and content SHA-256 used by Replay sessions.
Regenerate it with the current workspace Rust SDK using:

```powershell
cargo run -p rms_server --example generate_rms_replay_fixture
```

The SDK assigns fresh row and chunk identifiers on regeneration, so the generator prints the new file size and SHA-256.
After regeneration, update the HTTP contract expectations in `tests/api.rs` and run `rerun rrd verify`.

Replay and immutable Recording responses support HTTP byte ranges, `ETag`, and `If-Range` so the Web Viewer can seek without downloading an entire large file.
The Live fixture response remains `no-store`.

## Recording imports

`POST /api/v1/recording-imports` accepts one streaming multipart upload and returns `202 Accepted` with a durable `RecordingImport` in `processing` state.
Clients should send an `Idempotency-Key` of 1 to 128 printable characters.
After the source hash is known, the server durably binds that key to the project, device, optional source, format, mapping, and content hash; an exact retry returns the original import, while changed content or metadata returns `409 Conflict`.
Multipart fields must be ordered as `projectId`, `deviceId`, optional `dataSourceId`, `format`, optional CSV `mapping`, and finally `file`.
The supported format labels are `rrd`, `mcap`, `ros2-bag-zip`, `csv`, and `video`, where video accepts MP4, MOV, and WebM filenames.
CSV mapping is an optional JSON object; without it, numeric columns use a zero-based row sequence.
When `dataSourceId` is omitted, the server creates or reuses a device-scoped `protocol=file`, `status=ready` source and a project DataAssignment.
An explicitly selected source must also use the `file` protocol, so an import cannot replace Live topic metadata.

Use `GET /api/v1/recording-imports`, `GET /api/v1/recording-imports/{id}`, and `DELETE /api/v1/recording-imports/{id}` to list, poll, cancel, or delete imports.
The original upload is available from `/api/v1/recording-imports/{id}/artifact` with private, no-store byte-range responses while it remains retained.
Ready RRD recordings use private immutable byte-range responses through the existing `/rerun/recordings/{id}` and Replay session routes.
Deleting a ready import is rejected while an open ReplaySession references its Recording.

The development limits are configured with `RMS_MAX_UPLOAD_BYTES`, `RMS_STORAGE_QUOTA_BYTES`, `RMS_MAX_CONCURRENT_UPLOADS`, `RMS_MAX_CONCURRENT_IMPORTS`, `RMS_UPLOAD_IDLE_TIMEOUT_SECS`, and `RMS_UPLOAD_TOTAL_TIMEOUT_SECS`.
The multipart extractor also enforces a fixed 513 MiB wire cap so malformed headers and chunked bodies cannot bypass the streaming file limit.
Upload admission wait time counts against the configured total timeout.
The server stores source bytes under a generated fixed path, verifies the source hash, and atomically replaces a JSON registry after every catalog transition.
Startup recovery restores registry-referenced staged deletions and removes unreferenced trash or partial pre-registry uploads.
The registry file is flushed before replacement; Windows uses `MoveFileExW` with replace and write-through flags, while Unix flushes the parent directory after rename.
Filesystems and remote volumes may implement weaker guarantees, so production deployments should still use a transactional catalog appropriate to their power-loss requirements.

The bundled ffmpeg and rosbags adapters are a local-development integration boundary, not a production isolation boundary.
Production deployments must run conversion in a separate low-privilege worker or sandbox with an import-specific ACL and mount, networking disabled, and explicit CPU, RAM, disk, process, and execution-time limits.

The local HTTP boundary accepts only loopback Host names or IP addresses by default.
Browser requests with an Origin header must exactly match `http://127.0.0.1:4173` or `http://localhost:4173`, while Origin-less CLI requests remain available.
`RMS_ALLOWED_HOSTS` and `RMS_ALLOWED_ORIGINS` add comma-separated exact development values, and CORS preflight responses never use a wildcard origin.
Allowed cross-origin responses support credentials and the `Content-Type`, `Idempotency-Key`, and `X-RMS-Request-ID` request headers.

## Network discovery

`POST /api/v1/network-discovery-sessions` starts a bounded, user-requested LAN search and returns `202 Accepted` with its ephemeral session.
The server listens for allowlisted RMS, Rerun, and RTSP mDNS/DNS-SD services and sends bounded SSDP and ONVIF WS-Discovery multicast probes.
It never performs a CIDR or port sweep, follows an advertised URL, binds the HTTP API to the LAN, or registers a discovered candidate automatically.
The public candidate response contains only a generated ID, display name, category, state, last-seen time, source count, and Live capability; addresses, ports, paths, and protocols remain server-private.
Use `GET /api/v1/network-discovery-sessions/{session_id}` to poll the sanitized snapshot and `DELETE` on the same path to cancel the active search.
Verification is explicit and short-lived, re-runs discovery, and checks the candidate fingerprint and UDP source pin before issuing an approval token.
Approval is an atomic catalog mutation that creates a testing Integration, an offline and unknown-health Device, pending selected DataSources, and observe/operator project assignments.
Approval creates no Live session, Replay session, control lease, or command, and retries for the same fingerprint and input return the original receipt without duplicate assets.
Discovery sessions, candidates, and verification tokens are TTL-bound in-memory state; only approved catalog assets are durable.

Signed `_rms._tcp.local` Edge Agent advertisements use the same candidate, verification, and approval flow.
The server validates the Ed25519 advertisement, public-key-derived device ID, freshness, nonce replay, UDP source pin, and optional organization HMAC proof before exposing a sanitized candidate.
Organization-trusted discovery requires `RMS_EDGE_DISCOVERY_ORGANIZATION_ID`, `RMS_EDGE_DISCOVERY_KID`, and `RMS_EDGE_DISCOVERY_PSK_FILE` together; the signed organization must equal the discovery session and Project organization.
This fixture server accepts one organization discovery key set per process; isolate organizations in separate server instances or place a tenant-aware enrollment service in front of it.
Approval durably binds the Edge public key to the exact Integration, Device, and selected DataSources; it still creates observe-only assignments and no Live session or control capability.

## Edge Agent heartbeat

`POST /api/v1/edge-agents/heartbeats` accepts the enrolled Edge Agent's compact JSON health and ROS 2/MAVLink inventory.
Configure `RMS_EDGE_BEARER_TOKEN_FILE` with an absolute file containing 32 to 512 bytes and provision the same token file on the Agent.
Restrict heartbeat-token and discovery-PSK files to the service account (`0600` on Unix and a private service-account ACL on Windows).
Every request must use `Authorization: Bearer`, `X-RMS-Timestamp`, `X-RMS-Nonce`, and `X-RMS-Signature` headers.
The signature binds the exact body SHA-256, timestamp, and nonce; the server also verifies the device ID/public key binding, ±30-second clock window, nonce replay, and monotonic sequence per `(deviceId, bootId)`.
Unknown signed Agents are never auto-registered.
An accepted heartbeat updates only the previously approved Device and DataSources, materializes bounded topic metadata, and leaves adapter-only sources in `pending` state because discovery does not prove a consumable Rerun stream.
If heartbeats stop for ten seconds, the Device becomes offline, topics become stale, and any lease for that Device is revoked.
ROS 2 and MAVLink Edge adapters are observation-only, so the server refuses Edge control leases and commands until a separately authenticated actuator dispatcher returns a device acknowledgement.
The server also disables the fixture command receipt path by default; `RMS_ENABLE_SIMULATED_CONTROL=1` is an explicit local-development switch and must never be used as an actuator acknowledgement in production.

The development binary remains bound to loopback.
`RMS_BIND_ADDR` may select another loopback port for local supervision or tests, but non-loopback addresses are rejected.
For a deployed service, terminate TLS and authenticate user traffic in a hardened reverse proxy or service gateway, pass an exact configured Host through `RMS_ALLOWED_HOSTS`, and expose only the heartbeat route required by Edge Agents.
Do not bind this fixture backend directly to an untrusted LAN or the public Internet.

This fixture is development data and is not a substitute for a growing Redap stream, real simultaneous recording, or durable object storage.
