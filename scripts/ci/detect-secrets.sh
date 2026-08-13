#!/usr/bin/env bash
set -euo pipefail

readonly BASELINE="${DETECT_SECRETS_BASELINE:-.secrets.baseline}"
readonly DETECT_SECRETS_SPEC="${DETECT_SECRETS_SPEC:-git+https://github.com/ibm/detect-secrets.git@076672a9a01abdfc7ecee2e7d14f08cdccb73976}"
readonly TEMP_ROOT="${RUNNER_TEMP:-${TMPDIR:-/tmp}}"
readonly VENV="${DETECT_SECRETS_VENV:-${TEMP_ROOT}/detect-secrets-venv}"

if [[ ! -f "${BASELINE}" ]]; then
    if [[ "${GITHUB_ACTIONS:-false}" == "true" ]]; then
        echo "::notice::detect-secrets enforcement will start when ${BASELINE} is added"
    else
        echo "detect-secrets: ${BASELINE} not found; skipping"
    fi
    exit 0
fi

python3 -m venv "${VENV}"
PIP_DISABLE_PIP_VERSION_CHECK=1 \
    "${VENV}/bin/python" -m pip install --quiet "${DETECT_SECRETS_SPEC}"

git ls-files -z | xargs -0 \
    "${VENV}/bin/detect-secrets-hook" \
    --baseline "${BASELINE}" \
    --use-all-plugins \
    --fail-on-unaudited \
    --
