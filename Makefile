.PHONY: test-unit test-integration test-performance test-all build

build:
	cargo build

test-unit:
	cargo test

test-integration: build
	@echo "Running integration tests..."
	python3 test/integration/test_server.py
	python3 test/integration/test_headless.py

test-performance: build
	@echo "Running performance tests..."
	python3 test/performance/test_load.py

test-all: test-unit test-integration test-performance

mcp-setup:
	pip install -r mcp/requirements.txt

mcp-run:
	python3 mcp/server.py
