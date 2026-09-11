# SPDX-FileCopyrightText: Sam Ludford <samludford76@gmail.com>
# SPDX-License-Identifier: MIT

PREFIX ?= $(HOME)/.local
BIN     = $(PREFIX)/bin/kando-cosmic-helper
RULE    = $(HOME)/.config/cosmic/com.system76.CosmicSettings.WindowRules/v1/tiling_exception_custom

.PHONY: build install uninstall rule setup doctor service apparmor

build:
	cargo build --release

install: build
	install -Dm755 target/release/kando-cosmic-helper $(BIN)
	@echo "Installed $(BIN)"
	@echo "Run 'make setup' for the interactive first-time setup (rule, shortcut, workspaces)."

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

setup: install
	$(BIN) setup

doctor:
	$(BIN) doctor

## Optional: autostart Kando at login, restart it if it dies, and make shortcuts go
## through the supervised instance (see README "Self-healing").
service:
	install -Dm755 contrib/kando-menu $(PREFIX)/bin/kando-menu
	install -Dm644 contrib/kando.service $(HOME)/.config/systemd/user/kando.service
	systemctl --user daemon-reload
	systemctl --user enable --now kando.service
	@echo "Point your COSMIC shortcuts / Solaar rules at: kando-menu \"Menu Name\""

## Optional (Ubuntu 24.04+): let Kando's Electron sandbox create user namespaces.
apparmor:
	sudo install -Dm644 contrib/apparmor-kando /etc/apparmor.d/kando
	sudo apparmor_parser -r /etc/apparmor.d/kando

uninstall:
	rm -f $(BIN)
