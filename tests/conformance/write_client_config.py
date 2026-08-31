#!/usr/bin/env python3
"""Publish an isolated upstream-client conformance route for the dataplane."""

from __future__ import annotations

import argparse
import json
import os
from urllib.parse import urlparse

import msgpack
import redis


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
                        "tool_schemas": {name: {} for name in tool_names},
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
