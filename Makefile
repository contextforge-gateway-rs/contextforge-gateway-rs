.PHONY: help docker-prod compose-up compose-down docs-serve

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | sort | awk 'BEGIN {FS = ":.*?## "}; {printf "\033[36m%-15s\033[0m %s\n", $$1, $$2}'

docker-prod: ## Build production Docker image (contextforge-data-plane:latest) from docker/Dockerfile
	docker build -t contextforge-data-plane:latest -f docker/Dockerfile .

compose-up: ## Launch stack: nginx, control plane, redis, postgres, pgbouncer, dataplane, fast_time_server
	@docker image inspect dataplane:latest >/dev/null 2>&1 || { \
		echo "Image dataplane:latest not found. Run 'make docker-prod' first."; \
		exit 1; \
	}
	docker compose -f docker/docker-compose.yml up -d nginx control-plane redis postgres pgbouncer data-plane fast_time_server register_fast_time

compose-down: ## Tear down the stack
	docker compose -f docker/docker-compose.yml stop nginx control-plane redis postgres pgbouncer data-plane fast_time_server register_fast_time

docs-serve: ## Serve the wiki book locally at http://127.0.0.1:3000
	mdbook serve _context/wiki --hostname 127.0.0.1 --port 3000 --open
