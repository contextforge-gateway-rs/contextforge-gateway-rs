#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "${script_dir}/../.." && pwd)"
compose_file="${script_dir}/docker-compose.yml"

export MCP_CONFORMANCE_VERSION="${MCP_CONFORMANCE_VERSION:-0.2.0-alpha.11}"
export MCP_CONFORMANCE_SOURCE_SHA="${MCP_CONFORMANCE_SOURCE_SHA:-c321dd32035556e6769d3724a8ee97d87c3faaac}"
export MCP_CONFORMANCE_SPEC_VERSION="${MCP_CONFORMANCE_SPEC_VERSION:-2026-07-28}"
export MCP_CONFORMANCE_SERVER_ID="${MCP_CONFORMANCE_SERVER_ID:-3f33286667d34b65a31c3bafd30e4c21}"
export MCP_CONFORMANCE_SUITE_DIR="${MCP_CONFORMANCE_SUITE_DIR:-${repo_root}/.conformance-suite}"
export CF_CONTROLPLANE_IMAGE="${CF_CONTROLPLANE_IMAGE:-ghcr.io/ibm/mcp-context-forge:latest}"
export CF_DATAPLANE_IMAGE="${CF_DATAPLANE_IMAGE:-contextforge-data-plane:conformance}"
export MCP_CONFORMANCE_COLOR="${MCP_CONFORMANCE_COLOR:-auto}"

for command in curl docker git jq node npm; do
  if ! command -v "${command}" > /dev/null 2>&1; then
    echo "Required command not found: ${command}" >&2
    exit 1
  fi
done
docker compose version > /dev/null

if [ -e "${MCP_CONFORMANCE_SUITE_DIR}" ] && [ ! -d "${MCP_CONFORMANCE_SUITE_DIR}/.git" ]; then
  echo "MCP_CONFORMANCE_SUITE_DIR is not a git checkout: ${MCP_CONFORMANCE_SUITE_DIR}" >&2
  exit 1
fi

if [ ! -d "${MCP_CONFORMANCE_SUITE_DIR}/.git" ]; then
  echo "Checking out the official conformance suite."
  git clone --filter=blob:none \
    https://github.com/modelcontextprotocol/conformance.git \
    "${MCP_CONFORMANCE_SUITE_DIR}"
  git -C "${MCP_CONFORMANCE_SUITE_DIR}" checkout --detach "${MCP_CONFORMANCE_SOURCE_SHA}"
fi

suite_sha="$(git -C "${MCP_CONFORMANCE_SUITE_DIR}" rev-parse HEAD)"
if [ "${suite_sha}" != "${MCP_CONFORMANCE_SOURCE_SHA}" ]; then
  echo "Conformance checkout is at ${suite_sha}; expected ${MCP_CONFORMANCE_SOURCE_SHA}." >&2
  echo "Use a checkout at the pinned commit or set MCP_CONFORMANCE_SUITE_DIR." >&2
  exit 1
fi

(
  echo "Installing official conformance dependencies."
  cd "${MCP_CONFORMANCE_SUITE_DIR}"
  test "$(node -p "require('./package.json').version")" = "${MCP_CONFORMANCE_VERSION}"
  npm ci --ignore-scripts
)

state_dir="$(mktemp -d "${TMPDIR:-/tmp}/contextforge-conformance.XXXXXX")"
export GITHUB_ENV="${state_dir}/github-env"
export GITHUB_OUTPUT="${state_dir}/github-output"
touch "${GITHUB_ENV}" "${GITHUB_OUTPUT}"
mkdir -p "${repo_root}/conformance-results"
export MCP_CONFORMANCE_RESULTS_DIR
MCP_CONFORMANCE_RESULTS_DIR="$(mktemp -d "${repo_root}/conformance-results/run.XXXXXX")"

# shellcheck disable=SC2329 # Invoked by the trap below.
cleanup() {
  local status="$?"
  trap - EXIT INT TERM
  if [ "${status}" -ne 0 ]; then
    echo "Conformance run failed; printing live stack logs." >&2
    MCP_CONFORMANCE_TOKEN=diagnostics-only \
      docker compose -f "${compose_file}" logs --no-color || true
    if [ -f "${repo_root}/conformance-logs/reference-server.log" ]; then
      echo "Official fixture log:" >&2
      sed -n '1,240p' "${repo_root}/conformance-logs/reference-server.log" >&2
    fi
  fi
  MCP_CONFORMANCE_TOKEN="${MCP_CONFORMANCE_TOKEN:-cleanup-only}" \
    "${script_dir}/stop-live-stack.sh" || true
  rm -f -- "${GITHUB_ENV}" "${GITHUB_OUTPUT}"
  rmdir -- "${state_dir}"
  exit "${status}"
}
trap cleanup EXIT INT TERM

if [ "${MCP_CONFORMANCE_SKIP_PULL:-false}" != "true" ]; then
  MCP_CONFORMANCE_TOKEN=pull-only \
    docker compose -f "${compose_file}" pull redis fixture-proxy control-plane nginx
fi
echo "Starting the fixture and control plane."
MCP_CONFORMANCE_TOKEN=bootstrap-only \
  "${script_dir}/start-fixture-and-control-plane.sh"
echo "Registering the fixture through the control plane."
MCP_CONFORMANCE_TOKEN=bootstrap-only \
  "${script_dir}/register-fixture.sh"

set -a
# shellcheck disable=SC1090
source "${GITHUB_ENV}"
set +a

echo "Starting the dataplane and nginx."
"${script_dir}/start-dataplane-and-nginx.sh"
echo "Running MCP ${MCP_CONFORMANCE_SPEC_VERSION} conformance."
"${script_dir}/run-conformance.sh"

set +e
echo "Running scoped MCP ${MCP_CONFORMANCE_SPEC_VERSION} client conformance."
"${script_dir}/run-client-conformance.sh"
client_status="$?"
set -e

runner_status="$(sed -n 's/^status=//p' "${GITHUB_OUTPUT}" | tail -n 1)"
if [ -z "${runner_status}" ]; then
  echo "Conformance runner did not report a status." >&2
  exit 1
fi

set +e
if [ "${MCP_CONFORMANCE_BLESS:-false}" = "true" ]; then
  "${script_dir}/report-baseline-diff.sh" --bless "${MCP_CONFORMANCE_RESULTS_DIR}/server"
  report_status="$?"
  "${script_dir}/bless-client-baseline.sh" \
    "${MCP_CONFORMANCE_RESULTS_DIR}/client" \
    "${script_dir}/client-expected-failures.yml"
  client_baseline_status="$?"
  if [ "${client_baseline_status}" -eq 0 ]; then
    client_status=0
  fi
else
  "${script_dir}/report-baseline-diff.sh" "${MCP_CONFORMANCE_RESULTS_DIR}/server"
  report_status="$?"
  client_baseline_status=0
fi
set -e

if [ "${runner_status}" -ne 0 ] && [ "${report_status}" -eq 0 ]; then
  echo "Official runner status ${runner_status} contained no dataplane baseline mismatch."
fi
if [ "${client_baseline_status}" -ne 0 ]; then
  exit "${client_baseline_status}"
fi
if [ "${client_status}" -ne 0 ]; then
  exit "${client_status}"
fi
exit "${report_status}"
