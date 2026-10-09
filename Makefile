# strata — common development tasks. Run `make help` for the list.

CARGO   ?= cargo
BIN     := target/release/strata
PREFIX  ?= $(HOME)/.local

.DEFAULT_GOAL := help
.PHONY: help build release run test lint fmt fmt-check check ci install uninstall clean fonts media website website-dev

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

FONT_DIR := target/fonts
NERD_FONT := https://github.com/ryanoasis/nerd-fonts/releases/latest/download/JetBrainsMono.tar.xz

fonts: ## Download the font used to record docs media
	mkdir -p $(FONT_DIR)
	curl -sL $(NERD_FONT) | tar -xJ -C $(FONT_DIR) JetBrainsMonoNerdFontMono-Regular.ttf JetBrainsMonoNerdFontMono-Bold.ttf JetBrainsMonoNerdFontMono-Italic.ttf

media: ## Record the docs screenshots and GIFs (needs tmux)
	@test -f $(FONT_DIR)/JetBrainsMonoNerdFontMono-Regular.ttf || $(MAKE) fonts
	$(CARGO) xtask media

website: ## Build the documentation site into website/out
	cd website && npm ci && npm run build

website-dev: ## Serve the documentation site with live reload
	cd website && npm install && npm run dev

clean: ## Remove build artefacts
	$(CARGO) clean
