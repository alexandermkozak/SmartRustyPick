.PHONY: test-unit test-integration test-performance test-all bench bench-smoke perf-compare profile build run run-cli run-server container-build container-up container-down container-logs container-cli

CONTAINER_ENGINE ?= podman
IMAGE ?= localhost/smart-rusty-pick:latest
PYTHON ?= python3
# Baseline file for `make perf-compare`, produced by an earlier `make test-performance`.
BASE ?= baseline_metrics.json

build:
	cargo build

run: run-cli

run-cli: build
	./target/debug/smart-rusty-pick-cli

run-server: build
	./target/debug/smart-rusty-pick-server

test-unit:
	cargo test --workspace

# Each suite runs in its own temporary directory, so they never touch the
# working copy's db_storage/config.toml and can be run individually.
test-integration: build
	@echo "Running integration tests..."
	@rm -f integration_results.md
	$(PYTHON) test/integration/test_server.py
	$(PYTHON) test/integration/test_headless.py
	$(PYTHON) test/integration/test_security.py

test-performance: build
	@echo "Running performance tests..."
	@rm -f performance_results.md performance_metrics.json
	$(PYTHON) test/performance/test_load.py
	$(PYTHON) test/performance/test_concurrency.py

test-all: test-unit test-integration test-performance

# Criterion micro-benchmarks of the engine itself: record codec, query execution,
# sorting and persistence. Run these when changing the engine; the Python suites
# measure the same paths end to end, but far more coarsely.
bench:
	cargo bench -p smart-rusty-pick-core

# One iteration of every benchmark. Fast enough for CI, and it fails if a benchmark
# stops compiling or panics - the usual way benchmarks silently rot.
bench-smoke:
	cargo bench -p smart-rusty-pick-core -- --test

# Diff the metrics of two performance runs on the same machine.
perf-compare:
	$(PYTHON) scripts/compare_perf.py $(BASE) performance_metrics.json

profile:
	./scripts/profile.sh $(FILTER)

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
