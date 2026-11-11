ifeq ($(OS),Windows_NT)
CARGO ?= cargo
TOOLCHAIN ?= stable-msvc
BINARY := target/release/remote-web-desktop.exe
SETUP := powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts/setup.ps1
STOP := powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts/stop.ps1
else
CARGO ?= $(if $(wildcard $(HOME)/.cargo/bin/cargo),$(HOME)/.cargo/bin/cargo,cargo)
TOOLCHAIN ?= stable
BINARY := ./target/release/remote-web-desktop
SETUP := bash scripts/setup.sh
STOP := bash scripts/stop.sh
endif
HOST ?= 0.0.0.0
PORT ?= 8080
ARGS ?=

.DEFAULT_GOAL := help
.PHONY: help setup deps build run demo stop test clean macos-permissions macos-restart

help:
	@echo "make setup  - Install Rust and OS-specific native build dependencies"
	@echo "make build  - Build the release binary"
	@echo "make run    - Run the real desktop server (default: 0.0.0.0:8080)"
	@echo "make demo   - Run the demo server (default: 0.0.0.0:8080)"
	@echo "make stop   - Stop servers from this project, including the macOS app"
	@echo "make test   - Run Rust tests"
	@echo "make clean  - Remove build artifacts"
	@echo "make macos-permissions - Package the app and request macOS permissions"
	@echo "make macos-restart     - Restart the packaged app after granting permissions"
	@echo "Options: HOST=127.0.0.1 PORT=8081 ARGS='--codec jpeg'"
	@echo "Set RWD_PASSWORD in the environment to use a fixed password."

setup deps:
	$(SETUP)

build:
	"$(CARGO)" +$(TOOLCHAIN) build --release --locked

run: build
	"$(BINARY)" --host "$(HOST)" --port "$(PORT)" $(ARGS)

demo: build
	"$(BINARY)" --demo --host "$(HOST)" --port "$(PORT)" $(ARGS)

stop:
	$(STOP)

test:
	"$(CARGO)" +$(TOOLCHAIN) test --locked

clean:
	"$(CARGO)" +$(TOOLCHAIN) clean

macos-permissions: build
	bash scripts/macos-permissions.sh --host "$(HOST)" --port "$(PORT)" $(ARGS)

macos-restart:
	bash scripts/package-macos.sh stop
	bash scripts/package-macos.sh start --host "$(HOST)" --port "$(PORT)" $(ARGS)
