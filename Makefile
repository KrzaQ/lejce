# Lejce: a library that drives coding agents.
#
# Machine-specific values (the git remotes to push to) go in Makefile.local,
# which git ignores. See Makefile.local.example.

.PHONY: help build test format lint push hooks check-private clean
.DEFAULT_GOAL := help

REMOTES ?= origin

help: ## Show this help
	@grep -hE '^[a-zA-Z0-9_-]+:.*?## .*$$' $(MAKEFILE_LIST) | sort -t: -k1,1 | \
		awk 'BEGIN {FS = ":.*?## "; w = 12} { k[NR] = $$1; v[NR] = $$2; if (length($$1) > w) w = length($$1) } \
		     END { for (i = 1; i <= NR; i++) printf "  \033[36m%-*s\033[0m  %s\n", w, k[i], v[i] }'

build: ## Build the library
	cargo build

test: ## Run the tests
	cargo test

format: ## Format the code
	cargo fmt

lint: ## Run clippy
	cargo clippy --all-targets -- -D warnings

push: ## Push main to every remote in REMOTES (default origin)
	for r in $(REMOTES); do git push $$r main || exit 1; done

hooks: ## Install the git pre-commit hook that blocks private text
	ln -sf ../../scripts/check-private.sh .git/hooks/pre-commit

check-private: ## Check the staged changes for private text
	scripts/check-private.sh

clean: ## Remove build output
	cargo clean

-include Makefile.local
