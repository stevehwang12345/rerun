#!/usr/bin/env python3
"""Emit one bounded ROS 2 graph snapshot as newline-delimited JSON.

This helper is intentionally observation-only. It creates a graph participant, waits for
discovery to settle, reads graph metadata through rclpy, writes one JSON object, and exits.
It never creates a publisher, subscription, service, action, or command endpoint.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import time
from collections.abc import Iterable

SCHEMA = "rms.ros2.graph.v1"
ERROR_SCHEMA = "rms.ros2.error.v1"
MAX_JSON_BYTES = 1024 * 1024
MAX_NODES = 512
MAX_TOPICS = 2048
MAX_ENDPOINTS = 4096
MAX_TYPES_PER_TOPIC = 16
MAX_STRING_BYTES = 512
DEVICE_ID_PATTERN = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$")
ROS_NAME_PATTERN = re.compile(r"^/[A-Za-z0-9_/]*$")


class ProbeFailure(Exception):
    """A failure whose code and public message are safe to return to the supervisor."""

    def __init__(self, code: str, public_message: str) -> None:
        super().__init__(public_message)
        self.code = code
        self.public_message = public_message


def _bounded_text(value: object, fallback: str = "unknown") -> str:
    text = str(value) if value is not None else fallback
    encoded = text.encode("utf-8", errors="replace")
    if len(encoded) <= MAX_STRING_BYTES:
        return text
    return encoded[:MAX_STRING_BYTES].decode("utf-8", errors="ignore")


def _enum_name(value: object) -> str:
    name = getattr(value, "name", None)
    if name:
        return _bounded_text(name).lower()
    text = _bounded_text(value)
    return text.rsplit(".", maxsplit=1)[-1].lower()


def _duration_ns(value: object) -> int | None:
    nanoseconds = getattr(value, "nanoseconds", None)
    if isinstance(nanoseconds, int):
        return max(0, nanoseconds)
    seconds = getattr(value, "sec", None)
    nanos = getattr(value, "nanosec", None)
    if isinstance(seconds, int) and isinstance(nanos, int):
        return max(0, seconds * 1_000_000_000 + nanos)
    return None


def _qos(qos: object) -> dict[str, object]:
    # Keep only interoperable policy metadata. Endpoint GUIDs, locators, and addresses are
    # deliberately excluded from the schema.
    result: dict[str, object] = {
        "history": _enum_name(getattr(qos, "history", None)),
        "depth": max(0, int(getattr(qos, "depth", 0))),
        "reliability": _enum_name(getattr(qos, "reliability", None)),
        "durability": _enum_name(getattr(qos, "durability", None)),
        "liveliness": _enum_name(getattr(qos, "liveliness", None)),
    }
    deadline_ns = _duration_ns(getattr(qos, "deadline", None))
    lifespan_ns = _duration_ns(getattr(qos, "lifespan", None))
    lease_ns = _duration_ns(getattr(qos, "liveliness_lease_duration", None))
    if deadline_ns is not None:
        result["deadlineNs"] = deadline_ns
    if lifespan_ns is not None:
        result["lifespanNs"] = lifespan_ns
    if lease_ns is not None:
        result["livelinessLeaseDurationNs"] = lease_ns
    return result


def _endpoint(endpoint: object) -> dict[str, object]:
    return {
        "nodeName": _bounded_text(getattr(endpoint, "node_name", None)),
        "nodeNamespace": _bounded_text(getattr(endpoint, "node_namespace", None), "/"),
        "topicType": _bounded_text(getattr(endpoint, "topic_type", None)),
        "qos": _qos(getattr(endpoint, "qos_profile", None)),
    }


def _validate_prefix(value: str) -> str:
    if len(value.encode("utf-8")) > MAX_STRING_BYTES or not ROS_NAME_PATTERN.fullmatch(value):
        raise argparse.ArgumentTypeError("topic prefixes must be absolute ROS names")
    return value.rstrip("/") or "/"


def _validate_namespace(value: str) -> str:
    value = _validate_prefix(value)
    if value == "/":
        return "/rms"
    return value


def _validate_device_id(value: str) -> str:
    if not DEVICE_ID_PATTERN.fullmatch(value):
        raise argparse.ArgumentTypeError("device id must use the managed identifier format")
    return value


def _parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(add_help=False, allow_abbrev=False)
    parser.add_argument("--device-id", required=True, type=_validate_device_id)
    parser.add_argument("--domain-id", required=True, type=int, choices=range(233))
    parser.add_argument("--probe-namespace", default="/rms", type=_validate_namespace)
    parser.add_argument("--allow-prefix", action="append", default=[], type=_validate_prefix)
    parser.add_argument("--deny-prefix", action="append", default=[], type=_validate_prefix)
    parser.add_argument("--settle-ms", type=int, choices=range(100, 5001), default=750)
    parser.add_argument("--security-enclave", type=_validate_prefix)
    args = parser.parse_args(argv)
    if len(args.allow_prefix) > 64 or len(args.deny_prefix) > 64:
        raise ProbeFailure("invalid_configuration", "The ROS 2 graph filter is too large.")
    return args


def _matches_prefix(name: str, prefix: str) -> bool:
    return prefix == "/" or name == prefix or name.startswith(prefix + "/")


def _topic_allowed(name: str, allow: Iterable[str], deny: Iterable[str]) -> bool:
    allow_tuple = tuple(allow)
    return (not allow_tuple or any(_matches_prefix(name, item) for item in allow_tuple)) and not any(
        _matches_prefix(name, item) for item in deny
    )


def _security_summary(enclave: str | None) -> dict[str, object]:
    enabled = os.environ.get("ROS_SECURITY_ENABLE", "").lower() in {"true", "1"}
    strategy = os.environ.get("ROS_SECURITY_STRATEGY", "Permissive")
    if strategy not in {"Enforce", "Permissive"}:
        strategy = "unknown"
    return {
        "enabled": enabled,
        "strategy": strategy,
        "enclave": enclave,
    }


def _collect(args: argparse.Namespace) -> dict[str, object]:
    try:
        import rclpy
        from rclpy.utilities import get_rmw_implementation_identifier
    except (ImportError, ModuleNotFoundError) as err:
        raise ProbeFailure("ros_unavailable", "ROS 2 graph discovery is unavailable.") from err

    context = rclpy.context.Context()
    node = None
    try:
        rclpy.init(args=[], context=context, domain_id=args.domain_id)
        node_cli_args = ["--enclave", args.security_enclave] if args.security_enclave else None
        node = rclpy.create_node(
            "rms_edge_graph_probe",
            context=context,
            cli_args=node_cli_args,
            namespace=args.probe_namespace,
            use_global_arguments=False,
            enable_rosout=False,
            start_parameter_services=False,
        )
        deadline = time.monotonic() + args.settle_ms / 1000.0
        while time.monotonic() < deadline:
            rclpy.spin_once(node, timeout_sec=min(0.05, max(0.0, deadline - time.monotonic())))

        raw_nodes = []
        graph_with_enclaves = getattr(node, "get_node_names_and_namespaces_with_enclaves", None)
        if callable(graph_with_enclaves):
            raw_nodes = graph_with_enclaves()
        else:
            raw_nodes = [(name, namespace, None) for name, namespace in node.get_node_names_and_namespaces()]

        nodes: list[dict[str, object]] = []
        for raw in sorted(raw_nodes, key=lambda item: (item[1], item[0])):
            name, namespace = raw[0], raw[1]
            if name == "rms_edge_graph_probe" and namespace == args.probe_namespace:
                continue
            item: dict[str, object] = {
                "name": _bounded_text(name),
                "namespace": _bounded_text(namespace, "/"),
            }
            if len(raw) > 2 and raw[2]:
                item["enclave"] = _bounded_text(raw[2])
            nodes.append(item)
            if len(nodes) >= MAX_NODES:
                break

        endpoint_count = 0
        topics: list[dict[str, object]] = []
        for topic_name, topic_types in sorted(node.get_topic_names_and_types(), key=lambda item: item[0]):
            if not _topic_allowed(topic_name, args.allow_prefix, args.deny_prefix):
                continue
            publishers = []
            subscribers = []
            for endpoint in node.get_publishers_info_by_topic(topic_name):
                if endpoint_count >= MAX_ENDPOINTS:
                    break
                publishers.append(_endpoint(endpoint))
                endpoint_count += 1
            for endpoint in node.get_subscriptions_info_by_topic(topic_name):
                if endpoint_count >= MAX_ENDPOINTS:
                    break
                subscribers.append(_endpoint(endpoint))
                endpoint_count += 1
            topics.append({
                "name": _bounded_text(topic_name),
                "types": sorted({_bounded_text(item) for item in topic_types})[:MAX_TYPES_PER_TOPIC],
                "publishers": publishers,
                "subscribers": subscribers,
            })
            if len(topics) >= MAX_TOPICS or endpoint_count >= MAX_ENDPOINTS:
                break

        return {
            "schema": SCHEMA,
            "capturedAtUnixMs": int(time.time() * 1000),
            "deviceId": args.device_id,
            "domainId": args.domain_id,
            "probeNamespace": args.probe_namespace,
            "rmwImplementation": _bounded_text(get_rmw_implementation_identifier()),
            "security": _security_summary(args.security_enclave),
            "nodes": nodes,
            "topics": topics,
            "limitsReached": {
                "nodes": len(nodes) >= MAX_NODES,
                "topics": len(topics) >= MAX_TOPICS,
                "endpoints": endpoint_count >= MAX_ENDPOINTS,
            },
        }
    except ProbeFailure:
        raise
    except Exception as err:
        # Detailed middleware exceptions may contain hostnames, filesystem paths, or security
        # material. They are intentionally retained only as an exception chain in-process.
        raise ProbeFailure("graph_query_failed", "The ROS 2 graph could not be inspected.") from err
    finally:
        if node is not None:
            try:
                node.destroy_node()
            except Exception:
                pass
        try:
            if context.ok():
                rclpy.shutdown(context=context)
        except Exception:
            pass


def _write_record(record: dict[str, object]) -> None:
    payload = json.dumps(record, separators=(",", ":"), sort_keys=True, ensure_ascii=False).encode("utf-8")
    if len(payload) > MAX_JSON_BYTES:
        raise ProbeFailure("output_limit", "The ROS 2 graph is too large to inspect safely.")
    sys.stdout.buffer.write(payload + b"\n")
    sys.stdout.buffer.flush()


def main(argv: list[str] | None = None) -> int:
    try:
        args = _parse_args(sys.argv[1:] if argv is None else argv)
        _write_record(_collect(args))
        return 0
    except ProbeFailure as err:
        _write_record({"schema": ERROR_SCHEMA, "code": err.code, "message": err.public_message})
        return 2
    except SystemExit:
        _write_record({
            "schema": ERROR_SCHEMA,
            "code": "invalid_configuration",
            "message": "The ROS 2 graph probe configuration is invalid.",
        })
        return 2
    except Exception:
        _write_record({
            "schema": ERROR_SCHEMA,
            "code": "probe_failed",
            "message": "The ROS 2 graph probe failed safely.",
        })
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
