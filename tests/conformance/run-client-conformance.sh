#!/usr/bin/env bash
set -euo pipefail

: "${MCP_CONFORMANCE_SPEC_VERSION:?MCP_CONFORMANCE_SPEC_VERSION must be set}"
: "${MCP_CONFORMANCE_SUBJECT:?MCP_CONFORMANCE_SUBJECT must be set}"

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "${script_dir}/../.." && pwd)"
suite_dir="${MCP_CONFORMANCE_SUITE_DIR:-${repo_root}/.conformance-suite}"
results_root="${MCP_CONFORMANCE_RESULTS_DIR:-${repo_root}/conformance-results}"
results_dir="${MCP_CONFORMANCE_CLIENT_RESULTS_DIR:-${results_root}/client}"
compose_file="${script_dir}/docker-compose.yml"
baseline_file="${script_dir}/client-expected-failures.yml"
client_command="${script_dir}/client-under-test.sh"
scenarios=(tools_call request-metadata http-standard-headers http-custom-headers)

mkdir -p "${results_dir}"

# The control-plane publisher would overwrite the scenario-specific Redis
# config, and its own backend probes would contaminate the client observations.
docker compose -f "${compose_file}" stop control-plane > /dev/null

status=0
for scenario in "${scenarios[@]}"; do
  set +e
  (
    cd "${suite_dir}"
    npm start -- \
      client \
      --command "${client_command}" \
      --scenario "${scenario}" \
      --spec-version "${MCP_CONFORMANCE_SPEC_VERSION}" \
      --expected-failures "${baseline_file}" \
      --timeout 60000 \
      --output-dir "${results_dir}"
  )
  scenario_status="$?"
  set -e
  if [ "${scenario_status}" -ne 0 ]; then
    status=1
  fi
done

exit "${status}"
