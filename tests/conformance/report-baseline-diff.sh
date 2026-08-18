#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "${script_dir}/../.." && pwd)"
suite_dir="${MCP_CONFORMANCE_SUITE_DIR:-${repo_root}/.conformance-suite}"
tsx="${suite_dir}/node_modules/.bin/tsx"

if [ ! -x "${tsx}" ]; then
  echo "Conformance suite dependencies are not installed: ${tsx}" >&2
  exit 2
fi

NODE_OPTIONS="${NODE_OPTIONS:+${NODE_OPTIONS} }--disable-warning=DEP0205" \
  exec "${tsx}" "${script_dir}/report-baseline-diff.mjs" "$@"
