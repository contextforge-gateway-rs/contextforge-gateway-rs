#!/usr/bin/env bash
set -euo pipefail

: "${MCP_CONFORMANCE_SCENARIO:?MCP_CONFORMANCE_SCENARIO must be set by the conformance runner}"
: "${MCP_CONFORMANCE_PROTOCOL_VERSION:?MCP_CONFORMANCE_PROTOCOL_VERSION must be set by the conformance runner}"
: "${MCP_CONFORMANCE_SUBJECT:?MCP_CONFORMANCE_SUBJECT must be set}"

if [ "$#" -ne 1 ]; then
  echo "Usage: client-under-test.sh <scenario-server-url>" >&2
  exit 2
fi
if [ "${MCP_CONFORMANCE_PROTOCOL_VERSION}" != "2026-07-28" ]; then
  echo "Only MCP 2026-07-28 client conformance is supported" >&2
  exit 2
fi

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
compose_file="${script_dir}/docker-compose.yml"
virtual_host_id="${MCP_CONFORMANCE_CLIENT_SERVER_ID:-dataplane-client-conformance}"
conformance_port="${MCP_CONFORMANCE_PORT:-8080}"
scenario_server_url="$1"
backend_url="${scenario_server_url/\/\/localhost:/\/\/host.docker.internal:}"
backend_url="${backend_url/\/\/127.0.0.1:/\/\/host.docker.internal:}"

case "${MCP_CONFORMANCE_SCENARIO}" in
  tools_call)
    tool_calls='[{"name":"add_numbers","arguments":{"a":2,"b":3}}]'
    ;;
  request-metadata)
    tool_calls='[{"name":"metadata_probe","arguments":{}}]'
    ;;
  http-standard-headers)
    tool_calls='[{"name":"test_headers","arguments":{}}]'
    ;;
  http-custom-headers)
    : "${MCP_CONFORMANCE_CONTEXT:?MCP_CONFORMANCE_CONTEXT is required for http-custom-headers}"
    tool_calls="$(jq --exit-status --compact-output '.toolCalls' <<< "${MCP_CONFORMANCE_CONTEXT}")"
    ;;
  *)
    echo "Unsupported dataplane client conformance scenario: ${MCP_CONFORMANCE_SCENARIO}" >&2
    exit 2
    ;;
esac

tool_names="$(jq --exit-status --compact-output '[.[].name] | unique' <<< "${tool_calls}")"
prepared_tool_calls="$(docker compose -f "${compose_file}" run --rm --no-deps \
  --entrypoint python3 control-plane \
  /opt/contextforge-conformance/write_client_config.py \
  "${MCP_CONFORMANCE_SUBJECT}" \
  "${virtual_host_id}" \
  "${backend_url}" \
  "${tool_names}" \
  "${tool_calls}")"

endpoint="http://127.0.0.1:${conformance_port}/servers/${virtual_host_id}/mcp"
while IFS= read -r tool_call; do
  tool_name="$(jq --exit-status --raw-output '.name' <<< "${tool_call}")"
  arguments="$(jq --exit-status --compact-output '.arguments' <<< "${tool_call}")"
  header_args=()
  while IFS= read -r header; do
    header_args+=(--header "${header}")
  done < <(jq --exit-status --raw-output '.headers | to_entries[] | "\(.key): \(.value)"' <<< "${tool_call}")
  request="$(jq --null-input --compact-output \
    --arg name "${tool_name}" \
    --argjson arguments "${arguments}" \
    --arg version "${MCP_CONFORMANCE_PROTOCOL_VERSION}" \
    '{
      jsonrpc: "2.0",
      id: 1,
      method: "tools/call",
      params: {
        name: $name,
        arguments: $arguments,
        _meta: {
          "io.modelcontextprotocol/protocolVersion": $version,
          "io.modelcontextprotocol/clientInfo": {
            name: "dataplane-client-conformance-driver",
            version: "1.0.0"
          },
          "io.modelcontextprotocol/clientCapabilities": {}
        }
      }
    }')"

  response="$(curl --silent --show-error --fail-with-body \
    --request POST \
    --header 'Content-Type: application/json' \
    --header 'Accept: application/json, text/event-stream' \
    --header "MCP-Protocol-Version: ${MCP_CONFORMANCE_PROTOCOL_VERSION}" \
    --header 'MCP-Method: tools/call' \
    --header "MCP-Name: ${tool_name}" \
    "${header_args[@]}" \
    --data "${request}" \
    "${endpoint}")"

  response_json="$(sed -n 's/^data: //p' <<< "${response}" | head -n 1)"
  if [ -z "${response_json}" ]; then
    response_json="${response}"
  fi
  if ! jq --exit-status '.result != null and .error == null' <<< "${response_json}" > /dev/null; then
    echo "Dataplane rejected client conformance tool call ${tool_name}:" >&2
    echo "${response}" >&2
    exit 1
  fi
done < <(jq --compact-output '.[]' <<< "${prepared_tool_calls}")
