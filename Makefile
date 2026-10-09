# strata — common development tasks. Run `make help` for the list.

CARGO   ?= cargo
BIN     := target/release/strata
PREFIX  ?= $(HOME)/.local

.DEFAULT_GOAL := help
.PHONY: help build release run test lint fmt fmt-check check ci install uninstall clean

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-11s\033[0m %s\n", $$1, $$2}'

build: ## Debug build
	$(CARGO) build

release: ## Optimised build (target/release/strata)
	$(CARGO) build --release --locked

run: ## Run strata (pass ARGS="..." for arguments)
	$(CARGO) run -- $(ARGS)

test: ## Run all tests
	$(CARGO) test --workspace --all-features

lint: ## Clippy with warnings as errors
	$(CARGO) clippy --workspace --all-targets --all-features -- -D warnings

fmt: ## Format the code
	$(CARGO) fmt --all

fmt-check: ## Check formatting
	$(CARGO) fmt --all --check

check: fmt-check lint test ## Everything CI runs

ci: check ## Alias for check

install: release ## Install to $(PREFIX)/bin
	mkdir -p $(PREFIX)/bin
	install -m 755 $(BIN) $(PREFIX)/bin/strata

uninstall: ## Remove the installed binary
	rm -f $(PREFIX)/bin/strata

clean: ## Remove build artefacts
	$(CARGO) clean
