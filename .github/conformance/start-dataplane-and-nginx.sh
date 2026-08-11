#!/usr/bin/env bash
set -euo pipefail

: "${MCP_CONFORMANCE_SERVER_ID:?MCP_CONFORMANCE_SERVER_ID must be set}"

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "${script_dir}/../.." && pwd)"
log_dir="${repo_root}/conformance-logs"
conformance_port="${MCP_CONFORMANCE_PORT:-8080}"

docker compose -f "${script_dir}/docker-compose.yml" \
  up -d --wait data-plane nginx

endpoint="http://127.0.0.1:${conformance_port}/servers/${MCP_CONFORMANCE_SERVER_ID}/mcp"
request='{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "server/discover",
  "params": {
    "_meta": {
      "io.modelcontextprotocol/protocolVersion": "2026-07-28",
      "io.modelcontextprotocol/clientInfo": {
        "name": "ci-route-probe",
        "version": "1.0.0"
      },
      "io.modelcontextprotocol/clientCapabilities": {}
    }
  }
}'

for _ in $(seq 1 120); do
  curl --silent --show-error \
    --dump-header "${log_dir}/route-probe-headers.txt" \
    --output "${log_dir}/route-probe-body.txt" \
    --request POST \
    --header 'Content-Type: application/json' \
    --header 'Accept: application/json, text/event-stream' \
    --header 'MCP-Protocol-Version: 2026-07-28' \
    --header 'MCP-Method: server/discover' \
    --data "${request}" \
    "${endpoint}" || true
  if grep --ignore-case --quiet '^X-CF-Conformance-Backend: dataplane' \
    "${log_dir}/route-probe-headers.txt" \
    && sed -n 's/^data: //p' "${log_dir}/route-probe-body.txt" \
      | jq --exit-status \
        '.result.supportedVersions | index("2026-07-28") != null' \
        > /dev/null 2>&1; then
    exit 0
  fi
  sleep 0.5
done

echo "Modern MCP route did not reach the dataplane through nginx" >&2
cat "${log_dir}/route-probe-headers.txt" >&2
cat "${log_dir}/route-probe-body.txt" >&2
exit 1
