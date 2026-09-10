# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

# Build pipeline for the Hello Python Plugin example
#
# Usage:
#   make build    # Full build: web UI + embed + PyInstaller binary
#   make ui       # Build only the web UI
#   make embed    # Build UI + generate _embedded_ui.py
#   make binary   # PyInstaller compile (assumes _embedded_ui.py exists)
#   make test     # Run integration tests against the compiled binary
#   make clean    # Remove build artifacts
#   make dev      # Run the plugin directly with Python (no compile)

SDK_ROOT := ../..
EXAMPLE_DIR := $(shell pwd)
DIST_DIR := $(EXAMPLE_DIR)/dist
BINARY := $(DIST_DIR)/hello-python
WEB_DIR := $(EXAMPLE_DIR)/web
SCRIPTS_DIR := $(EXAMPLE_DIR)/scripts

.PHONY: build ui embed binary test clean dev

# Full build pipeline
build: embed binary
	@echo "Build complete: $(BINARY)"
	@ls -lh $(BINARY)

# Build the React web UI
ui:
	@echo "=== Building web UI ==="
	@cd $(WEB_DIR) && bun install && bun run build

# Build UI and generate _embedded_ui.py
embed:
	@echo "=== Embedding UI assets ==="
	@cd $(SDK_ROOT) && uv run python3 $(SCRIPTS_DIR)/embed_assets.py

# Compile to single binary with PyInstaller
binary:
	@echo "=== Compiling with PyInstaller ==="
	@cd $(SDK_ROOT) && uv run pyinstaller \
		--onefile \
		--name hello-python \
		--distpath $(DIST_DIR) \
		--workpath $(EXAMPLE_DIR)/build \
		--specpath $(EXAMPLE_DIR) \
		--paths . \
		--hidden-import _embedded_ui \
		--clean \
		$(EXAMPLE_DIR)/main.py
	@rm -rf $(EXAMPLE_DIR)/build $(EXAMPLE_DIR)/hello-python.spec
	@echo "Binary: $(BINARY) ($$(du -h $(BINARY) | cut -f1))"

# Run integration tests
test: build
	@echo "=== Running integration tests ==="
	@bun run $(SCRIPTS_DIR)/integration-test.ts

# Run directly with Python (for development)
dev:
	@cd $(SDK_ROOT) && uv run python3 $(EXAMPLE_DIR)/main.py \
		--socket-path /tmp/hello-python-dev.sock \
		--auth-secret dev-secret \
		--data-dir /tmp/hello-python-dev-data

# Clean build artifacts
clean:
	rm -rf $(DIST_DIR)
	rm -rf $(EXAMPLE_DIR)/build
	rm -rf $(WEB_DIR)/dist
	rm -rf $(WEB_DIR)/node_modules
	rm -f $(EXAMPLE_DIR)/_embedded_ui.py
	rm -f $(EXAMPLE_DIR)/*.spec
	@echo "Cleaned"
