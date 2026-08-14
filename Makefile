IMAGE_NAME := contextforge-data-plane:latest
CF_DATAPLANE_IMAGE ?= contextforge-data-plane:conformance
SERVICES ?= nginx control-plane redis postgres pgbouncer data-plane fast_time_server register_fast_time
ARGS     ?=

# IBM detect-secrets hardened fork — pinned to the same commit used in mcp-context-forge.
DETECT_SECRETS_SPEC ?= git+https://github.com/ibm/detect-secrets.git@076672a9a01abdfc7ecee2e7d14f08cdccb73976
DETECT_SECRETS_EXCLUDE := '(?x)(Cargo\.lock$$|\.lock$$)|^\.secrets\.baseline$$'

.PHONY: help docker-prod compose-up compose-down conformance docs-serve pre-commit secrets-scan-all configure-git

help: ## Show available commands
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | sort | awk 'BEGIN {FS = ":.*?## "}; {printf "\033[36m%-22s\033[0m %s\n", $$1, $$2}'

docker-prod: ## Build production Docker image (contextforge-data-plane:latest) from docker/Dockerfile
	docker build -t $(IMAGE_NAME) -f docker/Dockerfile .

compose-up: ## Launch stack: nginx, control plane, redis, postgres, pgbouncer, dataplane, fast_time_server
	@docker image inspect $(IMAGE_NAME) >/dev/null 2>&1 || { \
		echo "Image $(IMAGE_NAME) not found. Run 'make docker-prod' first."; \
		exit 1; \
	}
	docker compose -f docker/docker-compose.yml up -d $(SERVICES) $(ARGS)

compose-down: ## Tear down the stack
	docker compose -f docker/docker-compose.yml stop $(SERVICES) $(ARGS)

conformance: ## Build the data plane and run official MCP 2026-07-28 conformance locally
	docker build -t "$(CF_DATAPLANE_IMAGE)" -f docker/conformance.Dockerfile .
	CF_DATAPLANE_IMAGE="$(CF_DATAPLANE_IMAGE)" tests/conformance/run-local.sh

docs-serve: ## Serve the wiki book locally at http://127.0.0.1:3000
	mdbook serve _context/wiki --hostname 127.0.0.1 --port 3000 --open

pre-commit: ## Run all pre-commit hooks against every file
	@if ! command -v pre-commit >/dev/null 2>&1; then \
		echo "pre-commit not found. Install it with one of:"; \
		echo "  uv tool install pre-commit"; \
		echo "  brew install pre-commit"; \
		exit 1; \
	fi
	@mkdir -p .cache/pre-commit-home .cache/tmp .cache/cargo
	PRE_COMMIT_HOME='$(CURDIR)/.cache/pre-commit-home' \
	TMPDIR='$(CURDIR)/.cache/tmp' \
	CARGO_HOME='$(CURDIR)/.cache/cargo' \
	pre-commit run --config .pre-commit-config.yaml --all-files --show-diff-on-failure

secrets-scan-all: ## Full-tree scan — regenerate .secrets.baseline from scratch
	@if ! command -v detect-secrets >/dev/null 2>&1 && ! command -v uv >/dev/null 2>&1; then \
		echo "detect-secrets not found. Install it with:"; \
		echo "  uv tool install '$(DETECT_SECRETS_SPEC)'"; \
		exit 1; \
	fi
	@if command -v detect-secrets >/dev/null 2>&1; then \
		detect-secrets scan \
			--use-all-plugins \
			--exclude-files $(DETECT_SECRETS_EXCLUDE) \
			> .secrets.baseline; \
	else \
		uv tool run --from '$(DETECT_SECRETS_SPEC)' detect-secrets scan \
			--use-all-plugins \
			--exclude-files $(DETECT_SECRETS_EXCLUDE) \
			> .secrets.baseline; \
	fi
	@echo "✅ .secrets.baseline regenerated — audit new findings before committing"

# Internal target used by .gitattributes; intentionally omitted from `make help`.
configure-git:
	@common_dir=$$(git rev-parse --git-common-dir); \
	mkdir -p "$$common_dir/git-drivers"; \
	cp scripts/git/resolve-secrets-baseline-conflict.sh "$$common_dir/git-drivers/"; \
	chmod +x "$$common_dir/git-drivers/resolve-secrets-baseline-conflict.sh"; \
	git config merge.secrets-baseline.name "Regenerate .secrets.baseline via detect-secrets-scan"; \
	git config merge.secrets-baseline.driver \
		"$$common_dir/git-drivers/resolve-secrets-baseline-conflict.sh %O %A %B %P"
