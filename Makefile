.PHONY: test-unit test-integration test-performance test-all build run run-cli run-server

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
