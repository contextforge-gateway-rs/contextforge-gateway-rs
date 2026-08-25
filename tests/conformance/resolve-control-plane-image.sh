#!/usr/bin/env bash
set -euo pipefail

repository="IBM/mcp-context-forge"
image_repository="ghcr.io/ibm/mcp-context-forge"
api_url="https://api.github.com/repos/${repository}/commits?sha=main&per_page=100"
curl_args=(
  --fail
  --silent
  --show-error
  --retry 3
  --retry-all-errors
  --header "Accept: application/vnd.github+json"
  --header "X-GitHub-Api-Version: 2022-11-28"
)
if [ -n "${GITHUB_TOKEN:-}" ]; then
  curl_args+=(--header "Authorization: Bearer ${GITHUB_TOKEN}")
fi

commit_shas="$(curl "${curl_args[@]}" "${api_url}" | jq --exit-status --raw-output \
  '.[] | .sha | select(test("^[0-9a-f]{40}$"))')"

while IFS= read -r commit_sha; do
  image="${image_repository}:${commit_sha}"
  if docker manifest inspect "${image}" > /dev/null 2>&1; then
    echo "Resolved latest control-plane main image: ${image}" >&2
    echo "${image}"
    exit 0
  fi
done <<< "${commit_shas}"

echo "No published control-plane image found in the latest 100 main commits" >&2
exit 1
