.PHONY: test-unit test-integration test-performance test-all build run run-cli run-server container-build container-up container-down container-logs container-cli

CONTAINER_ENGINE ?= podman
IMAGE ?= localhost/smart-rusty-pick:latest

build:
	cargo build

run: run-cli

run-cli: build
	./target/debug/smart-rusty-pick-cli

run-server: build
	./target/debug/smart-rusty-pick-server

test-unit:
	cargo test --workspace

test-integration: build
	@echo "Running integration tests..."
	python3 test/integration/test_server.py
	python3 test/integration/test_headless.py
	python3 test/integration/test_security.py

test-performance: build
	@echo "Running performance tests..."
	python3 test/performance/test_load.py

test-all: test-unit test-integration test-performance

test-coverage:
	cargo llvm-cov --workspace --lcov --output-path lcov.info

test-coverage-html:
	cargo llvm-cov --workspace --html

mcp-setup:
	pip install -r mcp/requirements.txt

mcp-run:
	python3 mcp/server.py

container-build:
	$(CONTAINER_ENGINE) build -f Containerfile -t $(IMAGE) .

container-up:
	$(CONTAINER_ENGINE) compose up -d

container-down:
	$(CONTAINER_ENGINE) compose down

container-logs:
	$(CONTAINER_ENGINE) compose logs -f

container-cli:
	$(CONTAINER_ENGINE) exec -it smart-rusty-pick smart-rusty-pick-cli
