#!/usr/bin/env bash
set -euo pipefail

: "${GITHUB_OUTPUT:?GITHUB_OUTPUT must be set}"
: "${MCP_CONFORMANCE_SERVER_ID:?MCP_CONFORMANCE_SERVER_ID must be set}"
: "${MCP_CONFORMANCE_SPEC_VERSION:?MCP_CONFORMANCE_SPEC_VERSION must be set}"

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "${script_dir}/../.." && pwd)"
suite_dir="${MCP_CONFORMANCE_SUITE_DIR:-${repo_root}/.conformance-suite}"
conformance_port="${MCP_CONFORMANCE_PORT:-8080}"

set +e
(
  cd "${suite_dir}"
  npm start -- \
    server \
    --url "http://127.0.0.1:${conformance_port}/servers/${MCP_CONFORMANCE_SERVER_ID}/mcp" \
    --requirements "${MCP_CONFORMANCE_SPEC_VERSION}" \
    --expected-failures "${script_dir}/expected-failures.yml" \
    --output-dir "${repo_root}/conformance-results"
) 2>&1 | tee "${repo_root}/conformance-logs/runner.log"
runner_status="${PIPESTATUS[0]}"
set -e

echo "status=${runner_status}" >> "${GITHUB_OUTPUT}"
