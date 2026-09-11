# SPDX-FileCopyrightText: Sam Ludford <samludford76@gmail.com>
# SPDX-License-Identifier: MIT

PREFIX ?= $(HOME)/.local
BIN     = $(PREFIX)/bin/kando-cosmic-helper
RULE    = $(HOME)/.config/cosmic/com.system76.CosmicSettings.WindowRules/v1/tiling_exception_custom

.PHONY: build install uninstall rule

build:
	cargo build --release

install: build
	install -Dm755 target/release/kando-cosmic-helper $(BIN)
	@echo "Installed $(BIN)"
	@echo "Run 'make rule' to add the COSMIC floating-window exception for Kando."

## Adds the floating-window exception unless the file already mentions Kando.
rule:
	@mkdir -p $(dir $(RULE))
	@if grep -qs 'menu.kando.Kando' $(RULE); then \
		echo "$(RULE) already contains a Kando rule"; \
	elif [ -s $(RULE) ]; then \
		echo "$(RULE) exists; add the rule from data/tiling_exception_custom by hand"; \
	else \
		cp data/tiling_exception_custom $(RULE) && echo "Wrote $(RULE)"; \
	fi

uninstall:
	rm -f $(BIN)
