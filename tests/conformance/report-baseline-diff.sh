#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "${script_dir}/../.." && pwd)"
results_dir="${1:-${repo_root}/conformance-results}"
baseline_file="${2:-${script_dir}/expected-failures.yml}"
upstream_file="${3:-${script_dir}/upstream-fixture-failures.yml}"
suite_dir="${MCP_CONFORMANCE_SUITE_DIR:-${repo_root}/.conformance-suite}"
spec_version="${MCP_CONFORMANCE_SPEC_VERSION:-2026-07-28}"
requirements_file="${suite_dir}/requirements/${spec_version}.yaml"

for command in awk cut grep jq rg sed sort wc; do
  if ! command -v "${command}" > /dev/null 2>&1; then
    echo "Required command not found: ${command}" >&2
    exit 1
  fi
done

for required_file in "${baseline_file}" "${upstream_file}" "${requirements_file}"; do
  if [ ! -f "${required_file}" ]; then
    echo "Required conformance file not found: ${required_file}" >&2
    exit 1
  fi
done

state_dir="$(mktemp -d "${TMPDIR:-/tmp}/contextforge-baseline-diff.XXXXXX")"
actual_findings="${state_dir}/actual-findings.tsv"
actual_keys="${state_dir}/actual-keys.txt"
baseline_entries="${state_dir}/baseline-entries.txt"
upstream_entries="${state_dir}/upstream-entries.txt"
scored_scenarios="${state_dir}/scored-scenarios.txt"
unexpected_entries="${state_dir}/unexpected-entries.txt"
stale_entries="${state_dir}/stale-entries.txt"
upstream_matches="${state_dir}/upstream-matches.txt"

cleanup() {
  rm -f -- \
    "${actual_findings}" \
    "${actual_keys}" \
    "${baseline_entries}" \
    "${upstream_entries}" \
    "${scored_scenarios}" \
    "${unexpected_entries}" \
    "${stale_entries}" \
    "${upstream_matches}"
  rmdir -- "${state_dir}"
}
trap cleanup EXIT INT TERM

read_baseline() {
  awk '
    /^[[:space:]]*-[[:space:]]+/ {
      line = $0
      sub(/^[[:space:]]*-[[:space:]]+/, "", line)
      sub(/[[:space:]]+#.*$/, "", line)
      print line
    }
  ' "$1" | LC_ALL=C sort -u
}

awk '
  /^server:$/ { in_server = 1; next }
  in_server && /^[^[:space:]]/ { exit }
  in_server && /^[[:space:]]*-[[:space:]]+/ {
    line = $0
    sub(/^[[:space:]]*-[[:space:]]+/, "", line)
    print line
  }
' "${requirements_file}" | LC_ALL=C sort -u > "${scored_scenarios}"

read_baseline "${baseline_file}" > "${baseline_entries}"
read_baseline "${upstream_file}" > "${upstream_entries}"

if [ ! -d "${results_dir}" ]; then
  echo "::warning title=Conformance results missing::No results directory: ${results_dir}"
  if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    printf '## Conformance baseline diff\n\nNo conformance results were produced.\n' \
      >> "${GITHUB_STEP_SUMMARY}"
  fi
  exit 0
fi

: > "${actual_findings}"
while IFS= read -r -d '' checks_file; do
  case "${checks_file}" in
    */checks.json) ;;
    *) continue ;;
  esac

  result_name="$(basename -- "$(dirname -- "${checks_file}")")"
  if [[ ! "${result_name}" =~ ^server-(.*)-[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}-[0-9]{2}-[0-9]{2}-[0-9]{3}Z$ ]]; then
    echo "Skipping unrecognized result directory: ${result_name}" >&2
    continue
  fi
  scenario="${BASH_REMATCH[1]}"

  if ! grep --fixed-strings --line-regexp --quiet "${scenario}" "${scored_scenarios}"; then
    continue
  fi

  jq --raw-output --arg scenario "${scenario}" '
    .[] |
    select(.status == "FAILURE" or .status == "WARNING") |
    [($scenario + ":" + .id), .status, (.errorMessage // "")] |
    @tsv
  ' "${checks_file}" >> "${actual_findings}"
done < <(rg --files -uu -0 "${results_dir}")

LC_ALL=C sort -u -o "${actual_findings}" "${actual_findings}"
cut -f 1 "${actual_findings}" | LC_ALL=C sort -u > "${actual_keys}"

awk '
  NR == FNR { expected[$1] = 1; next }
  {
    scenario = $1
    sub(/:.*/, "", scenario)
    if (!(($1 in expected) || (scenario in expected))) print $1
  }
' "${baseline_entries}" "${actual_keys}" > "${unexpected_entries}"

awk '
  NR == FNR {
    actual[$1] = 1
    scenario = $1
    sub(/:.*/, "", scenario)
    actual_scenario[scenario] = 1
    next
  }
  !(($1 in actual) || ($1 in actual_scenario)) { print $1 }
' "${actual_keys}" "${baseline_entries}" > "${stale_entries}"

awk '
  NR == FNR { upstream[$1] = 1; next }
  {
    scenario = $1
    sub(/:.*/, "", scenario)
    if (($1 in upstream) || (scenario in upstream)) print $1
  }
' "${upstream_entries}" "${actual_keys}" > "${upstream_matches}"

line_count() {
  wc -l < "$1" | tr -d '[:space:]'
}

actual_count="$(line_count "${actual_keys}")"
baseline_count="$(line_count "${baseline_entries}")"
unexpected_count="$(line_count "${unexpected_entries}")"
stale_count="$(line_count "${stale_entries}")"
upstream_count="$(line_count "${upstream_matches}")"

echo "Conformance baseline diff (${spec_version})"
echo "  Actual scored findings: ${actual_count}"
echo "  Expected baseline entries: ${baseline_count}"
echo "  Matched pinned-fixture findings: ${upstream_count}"
echo "  Unexpected findings: ${unexpected_count}"
echo "  Stale baseline entries: ${stale_count}"

if [ "${unexpected_count}" -gt 0 ]; then
  echo
  echo "Unexpected findings (actual but not in baseline):"
  while IFS= read -r key; do
    detail="$(awk -F '\t' -v key="${key}" '$1 == key { print $2 ": " $3; exit }' "${actual_findings}")"
    echo "  - ${key} — ${detail}"
    echo "::error title=Unexpected conformance finding::${key}"
  done < "${unexpected_entries}"
fi

if [ "${stale_count}" -gt 0 ]; then
  echo
  echo "Stale baseline entries (expected but now passing):"
  while IFS= read -r key; do
    echo "  - ${key}"
    echo "::error title=Stale conformance baseline::${key} is now passing"
  done < "${stale_entries}"
fi

if [ "${unexpected_count}" -eq 0 ] && [ "${stale_count}" -eq 0 ]; then
  echo "Actual scored findings match the expected baseline."
fi

if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  {
    echo "## Conformance baseline diff"
    echo
    echo "| Category | Count |"
    echo "| --- | ---: |"
    echo "| Actual scored findings | ${actual_count} |"
    echo "| Expected baseline entries | ${baseline_count} |"
    echo "| Matched pinned-fixture findings | ${upstream_count} |"
    echo "| Unexpected findings | ${unexpected_count} |"
    echo "| Stale baseline entries | ${stale_count} |"
    echo
    echo "The pinned alpha.11 fixture has 7 scored failures and 1 warning recorded in \`upstream-fixture-failures.yml\`; its other 47 failures are extension or pending scenarios and are unscored."

    if [ "${unexpected_count}" -gt 0 ]; then
      echo
      echo "### Unexpected findings"
      while IFS= read -r key; do
        detail="$(awk -F '\t' -v key="${key}" '$1 == key { print $2 ": " $3; exit }' "${actual_findings}")"
        echo "- \`${key}\` — ${detail}"
      done < "${unexpected_entries}"
    fi

    if [ "${stale_count}" -gt 0 ]; then
      echo
      echo "### Stale baseline entries"
      while IFS= read -r key; do
        echo "- \`${key}\`"
      done < "${stale_entries}"
    fi

    if [ "${unexpected_count}" -eq 0 ] && [ "${stale_count}" -eq 0 ]; then
      echo
      echo "Actual scored findings match the expected baseline."
    fi
  } >> "${GITHUB_STEP_SUMMARY}"
fi
