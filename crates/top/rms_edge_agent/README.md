# RMS Edge Agent

`rms-edge-agent` is the observation-only network boundary between ROS 2 DDS or MAVLink devices and the RMS control plane.

One Agent identity represents exactly one RMS Device or gateway; run separate Agent instances with separate identity files for independently managed robots, drones, or vehicles.
It discovers protocol metadata, publishes a signed DNS-SD identity, and sends bounded signed health snapshots to RMS.
It never publishes ROS 2 topics, sends MAVLink commands, grants a control lease, or starts a live session.

## Trust boundaries

- The administration API binds a literal loopback address and requires a bearer token on every route, including health and metrics.
- The DNS-SD responder accepts only bounded `_rms._tcp.local` questions from private, link-local, or loopback sources.
- The advertised TCP port exposes only a bounded read-only manifest and nonce challenge API.
- Every advertisement and heartbeat is signed by a restart-stable Ed25519 identity.
- The device UUID is derived from the public key using SHA-256 and UUID version 8, so the server can validate the binding without a private key.
- Optional organization proof uses an HMAC key loaded from a file and identified only by a non-secret key ID.
- An advertisement without organization proof is suitable only for explicit operator-approved TOFU enrollment and observation access.
- Control enablement requires a separate RMS enrollment and authorization policy.

## Discovery contract

The agent advertises `_rms._tcp.local` with PTR, SRV, and TXT records.
The instance name is `<device-id>._rms._tcp.local`.
The SRV port is the read-only RMS manifest/challenge endpoint, not the administration API.
The server must discard the advertised hostname and pin the UDP datagram source address.

TXT fields are `v`, `id`, `pk`, `nonce`, `ts`, `name`, `kind`, `caps`, `path`, and `sig`.
When organization proof is provisioned, `org_kid` and `org_proof` are also present.
Capabilities are sorted before signing and currently include `ros2_dds` and/or `mavlink`.

The advertisement signature covers these exact bytes without a final newline:

```text
rms-advertisement-v1
service=_rms._tcp.local
instance=<lowercase instance fqdn>
id=<uuid>
nonce=<base64url nonce>
ts=<unix seconds>
port=<decimal port>
path=<manifest path>
kind=<device kind>
caps=<sorted comma-separated capabilities>
name=<display name>
```

If configured, organization proof is `HMAC-SHA256(PSK, canonical || "\norg=" || organization_id || "\norg_kid=" || org_kid)`.

## Configuration

All production file paths must be absolute.
Secrets are read from files and are never accepted inline.
The organization discovery PSK file contains 32–512 printable bytes, such as a provisioned base64 token, and those exact bytes are used as the HMAC key.
On Unix, the identity, token, MAVLink signing-key, and replay-state files must not grant group or other access.
On Windows, provision private ACLs that grant access only to the Edge Agent service account and required administrators; the agent validates regular-file boundaries but cannot infer deployment-specific account ACLs.
Secret-file symlinks follow the same policy as the common Edge secret loader: the resolved target must be a private regular file.
Replay-state symlinks are resolved to a private regular-file target and atomic updates preserve the link.
The example values must be replaced before deployment.

```toml
schema_version = 1
environment = "production"
identity_path = "/var/lib/rms-edge-agent/identity.json"

[admin]
bind_ip = "127.0.0.1"
port = 9878
bearer_token_path = "/etc/rms-edge-agent/admin.token"
max_connections = 32
request_timeout_ms = 2000

[advertise]
enabled = true
display_name = "Factory Gateway A"
device_kind = "gateway"
bind_ip = "192.168.10.20"
service_port = 9879
path = "/rms/v1/manifest"
ttl_seconds = 30
capabilities = ["mavlink", "ros2_dds"]
organization_id = "org-rms"
organization_key_id = "factory-a-2026"
organization_psk_path = "/etc/rms-edge-agent/discovery.psk"

[supervisor]
max_observations_per_adapter = 128
poll_timeout_ms = 2000
stale_after_ms = 10000
backoff_initial_ms = 250
backoff_max_ms = 30000
restart_limit = 5
restart_window_ms = 60000
event_capacity = 256

[control_plane]
enabled = true
server_url = "https://rms.example.internal/"
bearer_token_path = "/etc/rms-edge-agent/enrollment.token"
ca_certificate_path = "/etc/rms-edge-agent/rms-root-ca.pem"
interval_ms = 2000
timeout_ms = 1000
max_observations_per_adapter = 64

[[adapters]]
id = "factory-ros2"
kind = "ros2_dds"
enabled = true
poll_interval_ms = 2000

[adapters.settings]
deviceId = "factory-cell-a"
displayName = "Factory Cell A"
deviceKind = "robot"
domainId = 23
probeNamespace = "/rms_edge"
allowTopicPrefixes = ["/camera", "/diagnostics", "/tf"]
denyTopicPrefixes = ["/cmd_vel", "/control"]
settleMs = 750
staleAfterMs = 10000
discoveryRange = "subnet"

[[adapters]]
id = "flight-link"
kind = "mavlink"
enabled = true
poll_interval_ms = 2000

[adapters.settings]
bindIp = "192.168.10.20"
port = 14550
systemId = 7
allowedSourceIps = ["192.168.10.40"]
requireSigning = true
signingKeyPath = "/etc/rms-edge-agent/mavlink-signing.key"
replayStatePath = "/var/lib/rms-edge-agent/mavlink-replay.json"
receiveWindowMs = 750
heartbeatTimeoutMs = 5000
maxPeers = 128
signatureMaxAgeMs = 60000
signatureFutureSkewMs = 2000
```

The ROS 2 adapter invokes only the bundled fixed graph probe and does not accept a configurable executable or script path.
Use SROS2 settings where the DDS graph is protected.
The MAVLink adapter is passive and treats unsigned heartbeats as observations only.
`systemId` is required: one Edge Agent adapter represents one MAVLink vehicle while aggregating its components into one observation.
When signing is configured, `replayStatePath` is mandatory and must use an existing absolute parent directory.
`signingKeyPath` must be an absolute path resolving to an existing private regular file containing exactly 32 raw bytes.
Replay identity is `(systemId, componentId, linkId)`, independent of source IP, and the versioned replay state is atomically persisted so a process restart or routed source-address change cannot make an old signed frame fresh again.

## Running

```text
rms-edge-agent --config /etc/rms-edge-agent/agent.toml
```

`RMS_EDGE_CONFIG` may be used instead of `--config`.
Set `RUST_LOG` to configure structured tracing verbosity.
Send `SIGINT` or the platform service stop signal for graceful cancellation.

The authenticated loopback routes are:

- `GET /v1/status` for bounded adapter snapshots.
- `GET /v1/health` for aggregate readiness.
- `GET /metrics` for Prometheus/OpenMetrics text.

The public read-only pairing routes are:

- `GET <advertise.path>` for a fresh signed manifest.
- `GET <advertise.path>/ros2` and/or `<advertise.path>/mavlink` for capability-specific metadata endpoints.
- `POST /rms/v1/challenge` for a signed caller nonce.

## RMS heartbeat

The agent sends only the latest state and does not queue stale heartbeats.
Each process boot has a random `bootId`, and `sequence` is monotonic within that boot.
The server should key monotonic checks by `(deviceId, bootId)` and enforce timestamp and nonce replay windows.
When the control plane is enabled, validation keeps the heartbeat interval at five seconds or less and requires adapter polling, timeout, and heartbeat delivery to fit the server's ten-second freshness budget.
Authentication failures open a five-minute circuit and do not cause secret-file polling.
Transient failures use bounded exponential backoff with jitter.

The signature input is:

```text
rms-heartbeat-v1
<timestamp milliseconds>
<base64url nonce>
<lowercase SHA-256 hex of the exact JSON body>
```

## Deployment checklist

- Provision distinct admin, discovery, and enrollment secrets using the operating-system secret manager.
- Restrict identity and secret file ACLs to the service account.
- Bind the manifest endpoint to the intended private interface, never a wildcard or public address.
- Allow UDP 5353 only within the intended discovery segment.
- Require HTTPS with an internal CA for the RMS control plane.
- Configure SROS2 and MAVLink 2 signing wherever the device ecosystem supports them.
- Keep all discovered sources `metadata_only` until a separately authenticated stream is configured.
- Monitor adapter stale state, heartbeat failures, restart-budget exhaustion, and dropped discovery availability.
- Use an Edge Agent per broadcast domain when VLANs or Wi-Fi isolation block multicast.

## Verification

Run the focused checks before packaging:

```text
cargo fmt -p rms_edge_agent -- --check
cargo clippy -p rms_edge_agent --all-features --all-targets -- -D warnings
cargo nextest run --all-features --no-fail-fast -p rms_edge_agent
```
