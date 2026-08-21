#!/usr/bin/env python3
"""Publish an isolated upstream-client conformance route for the dataplane."""

from __future__ import annotations

import argparse
import json
import os
import urllib.request
from urllib.parse import urlparse

import msgpack
import redis

PROTOCOL_VERSION = "2026-07-28"


def fetch_tool_schemas(backend_url: str, tool_names: list[str]) -> dict[str, dict[str, object]]:
    body = json.dumps(
        {
            "jsonrpc": "2.0",
            "id": "control-plane-schema-discovery",
            "method": "tools/list",
            "params": {
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": PROTOCOL_VERSION,
                    "io.modelcontextprotocol/clientInfo": {
                        "name": "contextforge-conformance-control-plane",
                        "version": "1.0.0",
                    },
                    "io.modelcontextprotocol/clientCapabilities": {},
                }
            },
        }
    ).encode()
    request = urllib.request.Request(
        backend_url,
        data=body,
        headers={
            "Content-Type": "application/json",
            "Accept": "application/json, text/event-stream",
            "MCP-Protocol-Version": PROTOCOL_VERSION,
            "MCP-Method": "tools/list",
        },
        method="POST",
    )
    with urllib.request.urlopen(request, timeout=10) as response:
        response_body = response.read().decode()

    messages = [
        json.loads(line.removeprefix("data:").strip())
        for line in response_body.splitlines()
        if line.startswith("data:") and line.removeprefix("data:").strip()
    ]
    if not messages:
        messages = [json.loads(response_body)]
    tools = next(
        (
            message.get("result", {}).get("tools")
            for message in messages
            if isinstance(message.get("result", {}).get("tools"), list)
        ),
        None,
    )
    if tools is None:
        raise SystemExit(f"tools/list did not return tools: {response_body}")

    schemas = {
        tool["name"]: tool["inputSchema"]
        for tool in tools
        if isinstance(tool, dict)
        and tool.get("name") in tool_names
        and isinstance(tool.get("inputSchema"), dict)
    }
    missing = sorted(set(tool_names) - schemas.keys())
    if missing:
        raise SystemExit(f"tools/list omitted requested tool schemas: {missing}")
    return schemas


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("subject")
    parser.add_argument("virtual_host_id")
    parser.add_argument("backend_url")
    parser.add_argument("tool_names_json")
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    redis_url = os.environ.get("REDIS_URL")
    if not redis_url:
        raise SystemExit("REDIS_URL is required")

    parsed_url = urlparse(args.backend_url)
    if parsed_url.scheme not in {"http", "https"} or not parsed_url.hostname:
        raise SystemExit("backend_url must be an absolute HTTP(S) URL")

    tool_names = json.loads(args.tool_names_json)
    if (
        not isinstance(tool_names, list)
        or not tool_names
        or not all(isinstance(name, str) and name for name in tool_names)
    ):
        raise SystemExit("tool_names_json must be a non-empty JSON string array")

    backend_name = "conformance-backend"
    tool_schemas = fetch_tool_schemas(args.backend_url, tool_names)
    config = {
        "virtual_hosts": {
            args.virtual_host_id: {
                "backends": {
                    backend_name: {
                        "name": backend_name,
                        "url": args.backend_url,
                        "passthrough_headers": [],
                        "add_headers": {},
                        "remove_headers": [],
                        "allowed_tool_names": tool_names,
                        "tool_schemas": tool_schemas,
                        "tool_name_aliases": {},
                        "allowed_resource_names": [],
                        "allowed_prompt_names": [],
                    }
                }
            }
        }
    }

    key = msgpack.dumps(("UserConfig", args.subject), use_bin_type=True)
    value = msgpack.dumps(config, use_bin_type=True)
    client = redis.Redis.from_url(redis_url, decode_responses=False)
    client.set(key, value, ex=600)


if __name__ == "__main__":
    main()
