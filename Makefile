.DEFAULT_GOAL := help
SHELL := /bin/bash
.DELETE_ON_ERROR:

CARGO ?= cargo
BIN := target/release/mp4-concat-fuse
MOUNTPOINT ?= mnt
INPUTS ?= clips
NAME ?= merged.mp4
SPEC ?= spec.json
OUTPUT ?= merged.mp4
ARGS ?=
UNMOUNT := $(if $(filter Darwin,$(shell uname -s)),/sbin/umount,fusermount3 -u)

.PHONY: help build check test fmt lint install clean info mount mount-spec unmount export

help: ## Show shortcuts by group
	@printf '\nBuild & development\n'
	@printf '  make build          Build the release binary\n  make check          Check Rust code\n  make test           Run Rust tests\n  make fmt            Format Rust code\n  make lint           Run Clippy\n  make install        Install the binary with Cargo\n  make clean          Remove Cargo build output\n'
	@printf '\nUse the filesystem\n'
	@printf '  make info           Inspect INPUTS\n  make mount          Mount INPUTS at MOUNTPOINT (foreground)\n  make mount-spec     Mount files described by SPEC\n  make unmount        Unmount MOUNTPOINT\n  make export         Write INPUTS to OUTPUT\n'
	@printf '\nExample: make mount INPUTS="/media/card/DCIM" MOUNTPOINT="/tmp/trip"\n'
	@printf 'Multiple paths: make info INPUTS="clip1.mp4 clip2.mp4"\n'
	@printf 'For a path with spaces, use the CLI directly. ARGS adds CLI flags.\n\n'

build: ## Compile the CLI
	$(CARGO) build --release --locked

check:
	$(CARGO) check --locked

test:
	$(CARGO) test --locked

fmt:
	$(CARGO) fmt

lint:
	$(CARGO) clippy --locked --all-targets

install:
	$(CARGO) install --path . --locked

clean:
	$(CARGO) clean

info: build
	$(BIN) info $(INPUTS) $(ARGS)

mount: build
	mkdir -p "$(MOUNTPOINT)"
	$(BIN) mount "$(MOUNTPOINT)" $(INPUTS) --name "$(NAME)" $(ARGS)

mount-spec: build
	mkdir -p "$(MOUNTPOINT)"
	$(BIN) mount-spec "$(MOUNTPOINT)" "$(SPEC)" $(ARGS)

unmount:
	$(UNMOUNT) "$(MOUNTPOINT)"

export: build
	$(BIN) export "$(OUTPUT)" $(INPUTS) $(ARGS)
