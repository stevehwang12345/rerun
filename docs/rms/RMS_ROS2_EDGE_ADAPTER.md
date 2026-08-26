# RMS ROS 2/DDS Edge Adapter

## Service boundary

The ROS 2 adapter is an observation-only graph inventory service.
It does not publish DDS samples, subscribe to application data, call services, invoke actions, or acquire an RMS control lease.
Each poll creates one short-lived rclpy participant, waits for graph discovery, emits one bounded NDJSON snapshot, and exits.
The Rust edge agent supervises that child, enforces the deadline and output limits, and terminates the process tree during cancellation or failure.

The child reports ROS domain, namespace, nodes, topic types, publisher and subscriber associations, and interoperable QoS policies.
DDS endpoint GUIDs, locators, IP addresses, ports, raw packets, middleware diagnostics, and security key paths are excluded from its output contract.
The control plane receives a compact device observation and bounded topic inventory rather than raw DDS discovery internals.

## Deployment prerequisites

Install a supported ROS 2 distribution and its matching `rclpy` package on the edge host.
Source the distribution's managed setup before starting the service so `PYTHONPATH`, native library paths, and `AMENT_PREFIX_PATH` refer to the installed ROS runtime.
Provision a restart-stable `deviceId` during enrollment instead of deriving identity from a mutable hostname or network address.
Run the edge agent as a dedicated unprivileged operating-system account.
Keep its loopback administration token, device identity, and optional SROS2 keystore readable only by that account.

The adapter configuration does not accept a Python executable, script path, command line, or arbitrary environment map.
The probe source is compiled into the edge-agent binary and materialized as a private temporary file for each supervised poll.
ROS and operating-system environment variables are copied through a fixed allowlist.

## Example adapter settings

```toml
[[adapters]]
id = "primary-ros-graph"
kind = "ros2_dds"
enabled = true
poll_interval_ms = 2000

[adapters.settings]
deviceId = "robot-assembly-07"
displayName = "Assembly Robot 07"
deviceKind = "robot"
domainId = 42
probeNamespace = "/rms"
allowTopicPrefixes = ["/camera", "/lidar", "/tf", "/robot"]
denyTopicPrefixes = ["/robot/private"]
settleMs = 750
staleAfterMs = 15000
rmwImplementation = "rmw_cyclonedds_cpp"
discoveryRange = "subnet"
```

`domainId` accepts 0 through 232, while 0 through 101 is the ROS documentation's platform-compatible range.
`allowTopicPrefixes` is inclusive when non-empty, and `denyTopicPrefixes` always wins.
Filters are bounded literal ROS path prefixes and are not regular expressions.
The Rust adapter repeats filter validation after parsing the child snapshot, so a faulty or compromised child cannot bypass the configured topic boundary.

## SROS2 fail-closed configuration

```toml
[adapters.settings.security]
keystore = "/var/lib/rms/sros2"
enclave = "/rms/edge/discovery"
strategy = "enforce"
```

The keystore must be an existing absolute directory.
The enclave should contain a dedicated identity whose signed policy grants only the discovery access needed by the chosen RMW implementation.
Production deployments should use `enforce` so missing, invalid, or unauthorized DDS Security material prevents discovery.
`permissive` exists for controlled commissioning and produces only an observed, not authenticated, RMS result.
The child never returns the keystore path or key material.

## Freshness and restart behavior

Every successful poll replaces the previous ROS graph snapshot atomically in the supervisor.
Each device observation has an explicit expiry derived from `staleAfterMs`.
Transient probe failures move the adapter through degraded and stale states under the shared bounded exponential-backoff and restart-window policy.
Expired observations are not presented as online devices.
Configuration failures such as a missing ROS runtime or SROS2 keystore require operator action and are reported with a fixed, non-sensitive message.
Shutdown cancellation kills the complete probe process group before the adapter stops.

## Source and viewer mapping

The adapter groups supported topic types into camera, spatial, transform, trajectory, pose, telemetry, diagnostics, and generic-message inventories.
Images receive the image viewer hint, point clouds receive the point-cloud hint, pose and transform data receive the spatial hint, numeric sensor data receives the plot hint, and diagnostics receive the log hint.
All ROS graph sources have `metadata_only` status because graph presence does not prove that an RMS ingest subscription or Replay recording path is provisioned.
An independent, authenticated ingest adapter must be configured and verified before a source can become ready.

The ordinary operator screen should initially show only device name, connection health, available data categories, and project assignment.
Topic paths, message types, and QoS are available in the topic configuration drawer when the operator needs them.
Raw DDS endpoint information is never part of the UI DTO.

## Resource limits

One probe record is limited to 1 MiB and one diagnostic stream is limited to 16 KiB.
A snapshot is limited to 512 nodes, 2,048 topics, 4,096 endpoints, 16 message types per topic, and 64 allow or deny prefixes.
The adapter process deadline is the smaller of the supervisor deadline and ten seconds.
The source inventory is reduced to at most 16 source groups and 256 topics per group before entering the shared adapter contract.

## Verification without ROS in CI

Rust tests start a real fake Python probe process using fixed arguments and the same restricted environment and output parser used in production.
Parser tests reject malformed UTF-8, missing record terminators, multiple records, unknown fields, oversized output, filter bypasses, identity mismatch, and child errors containing sensitive-looking paths or addresses.
ROS-enabled staging should additionally validate graph convergence, multicast isolation behavior, each supported RMW implementation, SROS2 enforcement, and observation expiry after participants leave the graph.

## Official design references

- [ROS on DDS](https://design.ros2.org/articles/ros_on_dds.html)
- [ROS 2 domain identifiers](https://docs.ros.org/en/lyrical/Concepts/Intermediate/About-Domain-ID.html)
- [ROS 2 dynamic discovery controls](https://docs.ros.org/en/rolling/Tutorials/Advanced/Improved-Dynamic-Discovery.html)
- [ROS 2 QoS policies](https://docs.ros.org/en/humble/Concepts/Intermediate/About-Quality-of-Service-Settings.html)
- [ROS 2 DDS-Security integration](https://design.ros2.org/articles/ros2_dds_security.html)
- [ROS 2 security enclaves](https://design.ros2.org/articles/ros2_security_enclaves.html)
- [rcl graph API](https://docs.ros.org/en/rolling/p/rcl/generated/file_include_rcl_graph.h.html)
- [rclpy initialization and node creation](https://docs.ros.org/en/ros2_packages/rolling/api/rclpy/api/init_shutdown.html)
