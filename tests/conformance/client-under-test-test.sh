#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
state_dir="$(mktemp -d "${TMPDIR:-/tmp}/contextforge-client-adapter-test.XXXXXX")"
fake_bin="${state_dir}/bin"
docker_args="${state_dir}/docker-args"
curl_bodies="${state_dir}/curl-bodies"

cleanup() {
  rm -rf -- "${state_dir}"
}
trap cleanup EXIT INT TERM

mkdir -p "${fake_bin}"
cat > "${fake_bin}/docker" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" > "${FAKE_DOCKER_ARGS}"
EOF
cat > "${fake_bin}/curl" <<'EOF'
#!/usr/bin/env bash
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--data" ]; then
    shift
    printf '%s\n' "$1" >> "${FAKE_CURL_BODIES}"
  fi
  shift
done
printf '%s\n' 'data: {"jsonrpc":"2.0","id":1,"result":{"content":[]}}'
EOF
chmod +x "${fake_bin}/docker" "${fake_bin}/curl"

export PATH="${fake_bin}:${PATH}"
export FAKE_DOCKER_ARGS="${docker_args}"
export FAKE_CURL_BODIES="${curl_bodies}"
export MCP_CONFORMANCE_PROTOCOL_VERSION=2026-07-28
export MCP_CONFORMANCE_SUBJECT=test-subject
export MCP_CONFORMANCE_CLIENT_SERVER_ID=test-client-server
export MCP_CONFORMANCE_PORT=18080
export MCP_CONFORMANCE_SCENARIO=http-custom-headers
export MCP_CONFORMANCE_CONTEXT='{
  "name": "http-custom-headers",
  "toolCalls": [
    {"name": "first", "arguments": {"region": "west"}},
    {"name": "second", "arguments": {"verbose": null}}
  ]
}'

"${script_dir}/client-under-test.sh" "http://localhost:43123/mcp"

grep --fixed-strings --quiet -- 'http://host.docker.internal:43123/mcp' "${docker_args}"
grep --fixed-strings --quiet -- '["first","second"]' "${docker_args}"
test "$(wc -l < "${curl_bodies}" | tr -d '[:space:]')" -eq 2
jq --exit-status --slurp '
  length == 2 and
  .[0].method == "tools/call" and
  .[0].params.name == "first" and
  .[0].params.arguments.region == "west" and
  .[0].params._meta["io.modelcontextprotocol/protocolVersion"] == "2026-07-28" and
  .[1].params.name == "second" and
  .[1].params.arguments.verbose == null
' "${curl_bodies}" > /dev/null

echo 'client conformance adapter tests passed'
