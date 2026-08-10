.PHONY: help docker-prod testing-up testing-down

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | sort | awk 'BEGIN {FS = ":.*?## "}; {printf "\033[36m%-15s\033[0m %s\n", $$1, $$2}'

docker-prod: ## Build production Docker image (contextforge-data-plane:latest) from docker/Dockerfile
	docker build -t contextforge-data-plane:latest -f docker/Dockerfile .

testing-up: ## Launch testing stack: nginx, control plane, redis, postgres, pgbouncer, dataplane, fast_time_server
	@docker image inspect contextforge-data-plane:latest >/dev/null 2>&1 || { \
		echo "Image contextforge-data-plane:latest not found. Run 'make docker-prod' first."; \
		exit 1; \
	}
	docker compose -f docker/docker-compose.yml up -d nginx control-plane redis postgres pgbouncer data-plane fast_time_server register_fast_time

testing-down: ## Tear down the testing stack
	docker compose -f docker/docker-compose.yml stop nginx control-plane redis postgres pgbouncer data-plane fast_time_server register_fast_time
