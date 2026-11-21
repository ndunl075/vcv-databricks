# Shopping Sessions: build, test, and check exactly as the scorer would.
IMAGE    ?= shopping-sessions:latest
PLATFORM ?= linux/amd64
CPUS     ?= 8
MEM      ?= 24g
VENV     ?= $(HOME)/Developer/vcv-data/venv
PY       ?= $(if $(wildcard $(VENV)/bin/python),$(VENV)/bin/python,python3)
DEV      ?= $(HOME)/Developer/vcv-data/db/dev
ANSWER   ?= kit/dev-answer/dev-answer
# Colima/Docker Desktop only share $HOME with containers, so temp dirs must live under it.
TMPDIR   = $(HOME)/Developer/vcv-data/tmp
export IMAGE CPUS MEM PLATFORM TMPDIR

.PHONY: build test fixtures check-dev docker-build docker-check venv

## Native release build (fast dev loop; needs rustup)
build:
	cargo build --release

## Rust unit tests (timestamps, dialect, chunking, links)
test:
	cargo test --release

## Python checker deps (polars) for fixtures and compare
venv:
	python3 -m venv $(VENV) && $(VENV)/bin/pip install --only-binary :all: polars pyarrow

## All fixtures (starter + ours) against the native binary
fixtures: build
	cd tests && $(abspath $(PY)) run_fixtures.py fixtures -- ../target/release/ss

## Dev set against the native binary
check-dev: build
	rm -rf out && mkdir -p out && target/release/ss $(DEV) out
	cd tests && $(abspath $(PY)) compare.py ../out ../$(ANSWER)

## Build the image the way the scorer does (root Dockerfile, linux/amd64)
docker-build:
	docker buildx build --platform $(PLATFORM) --load -t $(IMAGE) .

## One command: image build, every fixture and the dev set inside the image, under the scorer's limits
docker-check: docker-build
	mkdir -p $(TMPDIR)
	cd tests && $(abspath $(PY)) run_fixtures.py fixtures -- ./docker_run.sh
	rm -rf out && time tests/docker_run.sh $(DEV) out
	cd tests && $(abspath $(PY)) compare.py ../out ../$(ANSWER)
	@docker image inspect $(IMAGE) --format 'image size: {{.Size}} bytes'
