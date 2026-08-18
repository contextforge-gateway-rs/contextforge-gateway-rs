#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "${script_dir}/../.." && pwd)"
reporter="${script_dir}/report-baseline-diff.sh"
state_dir="$(mktemp -d "${TMPDIR:-/tmp}/contextforge-baseline-test.XXXXXX")"
suite_dir="${MCP_CONFORMANCE_SUITE_DIR:-${repo_root}/.conformance-suite}"
results_dir="${state_dir}/results"
baseline_file="${state_dir}/expected-failures.yml"
upstream_file="${state_dir}/upstream-fixture-failures.yml"
summary_file="${state_dir}/summary.md"

cleanup() {
  rm -rf -- "${state_dir}"
}
trap cleanup EXIT INT TERM

if [ ! -x "${suite_dir}/node_modules/.bin/tsx" ]; then
  echo "Conformance suite dependencies are not installed: ${suite_dir}" >&2
  exit 2
fi

mkdir -p "${results_dir}"

cat > "${baseline_file}" <<'EOF'
server:
  - server-stateless:known
  - completion-complete
  - tools-call-simple-text:fixed
  - tools-call-image
  - tools-call-audio:not-emitted
EOF

cat > "${upstream_file}" <<'EOF'
server:
  - tools-call-embedded-resource:fixture-defect
EOF

write_checks() {
  scenario="$1"
  checks="$2"
  result_dir="${results_dir}/server-${scenario}-2026-08-18T12-00-00-000Z"
  mkdir -p "${result_dir}"
  printf '%s\n' "${checks}" > "${result_dir}/checks.json"
}

write_checks server-stateless '[{"id":"known","status":"FAILURE"}]'
write_checks completion-complete '[{"id":"any-failure","status":"WARNING"}]'
write_checks tools-list '[{"id":"new-failure","status":"FAILURE"}]'
write_checks tools-call-simple-text '[{"id":"fixed","status":"SUCCESS"}]'
write_checks tools-call-image '[{"id":"all-good","status":"SUCCESS"}]'
write_checks tools-call-audio '[{"id":"other","status":"SUCCESS"}]'
write_checks tools-call-embedded-resource '[{"id":"fixture-defect","status":"FAILURE","errorMessage":"must not be reported as a dataplane failure"}]'
write_checks tools-call-mixed-content '[{"id":"repeated","status":"FAILURE"},{"id":"repeated","status":"SUCCESS"}]'
write_checks tools-call-error '[{"id":"good","status":"SUCCESS"},{"id":"not-applicable","status":"SKIPPED"}]'

assert_contains() {
  haystack="$1"
  needle="$2"
  if [[ "${haystack}" != *"${needle}"* ]]; then
    echo "Expected output to contain: ${needle}" >&2
    echo "${haystack}" >&2
    exit 1
  fi
}

assert_not_contains() {
  haystack="$1"
  needle="$2"
  if [[ "${haystack}" == *"${needle}"* ]]; then
    echo "Expected output not to contain: ${needle}" >&2
    echo "${haystack}" >&2
    exit 1
  fi
}

set +e
output="$(
  GITHUB_ACTIONS=true \
  GITHUB_STEP_SUMMARY="${summary_file}" \
  MCP_CONFORMANCE_COLOR=never \
  MCP_CONFORMANCE_SUITE_DIR="${suite_dir}" \
    "${reporter}" "${results_dir}" "${baseline_file}" "${upstream_file}" 2>&1
)"
status="$?"
set -e

if [ "${status}" -ne 1 ]; then
  echo "Expected mismatch status 1, got ${status}" >&2
  echo "${output}" >&2
  exit 1
fi

assert_contains "${output}" 'XFAIL server-stateless:known'
assert_contains "${output}" 'XFAIL completion-complete'
assert_contains "${output}" 'UPSTREAM tools-call-embedded-resource:fixture-defect'
assert_contains "${output}" 'FAIL tools-call-mixed-content:repeated (expected PASS, got FAILURE)'
assert_contains "${output}" 'FAIL tools-list:new-failure (expected PASS, got FAILURE)'
assert_contains "${output}" 'XPASS tools-call-simple-text:fixed (expected FAILURE, got PASS)'
assert_contains "${output}" 'XPASS tools-call-image (expected FAILURE, got PASS)'
assert_not_contains "${output}" 'XPASS tools-call-audio:not-emitted'
assert_not_contains "${output}" '::error title=Expected conformance pass failed::tools-call-embedded-resource:fixture-defect'

summary="$(cat "${summary_file}")"
assert_contains "${summary}" '| Pinned fixture findings ignored | 1 |'
assert_not_contains "${summary}" 'tools-call-embedded-resource:fixture-defect'

cat > "${state_dir}/unmatched-upstream.yml" <<'EOF'
server:
  - resources-list:fixture-defect
EOF
set +e
unmatched_upstream_output="$(
  MCP_CONFORMANCE_COLOR=never \
  MCP_CONFORMANCE_SUITE_DIR="${suite_dir}" \
    "${reporter}" \
      "${results_dir}" \
      "${baseline_file}" \
      "${state_dir}/unmatched-upstream.yml" 2>&1
)"
unmatched_upstream_status="$?"
set -e
if [ "${unmatched_upstream_status}" -ne 1 ]; then
  echo "Expected unmatched-upstream status 1, got ${unmatched_upstream_status}" >&2
  exit 1
fi
assert_contains "${unmatched_upstream_output}" 'FAIL tools-call-embedded-resource:fixture-defect'

echo 'server: []' > "${state_dir}/empty-baseline.yml"
set +e
empty_baseline_output="$(
  MCP_CONFORMANCE_COLOR=never \
  MCP_CONFORMANCE_SUITE_DIR="${suite_dir}" \
    "${reporter}" \
      "${results_dir}" \
      "${state_dir}/empty-baseline.yml" \
      "${upstream_file}" 2>&1
)"
empty_baseline_status="$?"
set -e
if [ "${empty_baseline_status}" -ne 1 ]; then
  echo "Expected empty-baseline status 1, got ${empty_baseline_status}" >&2
  exit 1
fi
assert_contains "${empty_baseline_output}" 'FAIL tools-list:new-failure'

bless_output="$(
  MCP_CONFORMANCE_COLOR=never \
  MCP_CONFORMANCE_SUITE_DIR="${suite_dir}" \
    "${reporter}" --bless "${results_dir}" "${baseline_file}" "${upstream_file}"
)"
assert_contains "${bless_output}" "BLESS updated ${baseline_file}"

cat > "${state_dir}/expected-after-bless.yml" <<'EOF'
# Generated by `make conformance-bless` from scored dataplane findings.
# Pinned fixture findings are excluded; see upstream-fixture-failures.yml.
server:
  - completion-complete:any-failure
  - server-stateless:known
  - tools-call-mixed-content:repeated
  - tools-list:new-failure
EOF
diff -u "${state_dir}/expected-after-bless.yml" "${baseline_file}"

MCP_CONFORMANCE_COLOR=never \
MCP_CONFORMANCE_SUITE_DIR="${suite_dir}" \
  "${reporter}" "${results_dir}" "${baseline_file}" "${upstream_file}" > /dev/null

baseline_before_missing="$(cat "${baseline_file}")"
set +e
MCP_CONFORMANCE_COLOR=never \
MCP_CONFORMANCE_SUITE_DIR="${suite_dir}" \
  "${reporter}" --bless "${state_dir}/missing" "${baseline_file}" "${upstream_file}" > /dev/null 2>&1
missing_status="$?"
set -e

if [ "${missing_status}" -ne 2 ]; then
  echo "Expected missing-results status 2, got ${missing_status}" >&2
  exit 1
fi
if [ "$(cat "${baseline_file}")" != "${baseline_before_missing}" ]; then
  echo 'Bless changed the baseline without results' >&2
  exit 1
fi

echo 'conformance reporter tests passed'
