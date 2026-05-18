# tmux port Makefile (Phase 69d Track D.2)
#
# Host-side build via `cargo xtask port build tmux`. Depends on the
# ncurses (wide) and libevent ports.

PORTDIR := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))
PREFIX ?= /usr/local
WORKDIR := $(PORTDIR)/work

.PHONY: fetch patch build install clean

fetch:
	@echo "tmux: in-OS fetch path deferred — see xtask port build" && exit 1

patch:
	@if [ -d "$(PORTDIR)/patches" ]; then \
		for p in $(PORTDIR)/patches/*.patch; do \
			[ -f "$$p" ] && patch -d $(WORKDIR) -p1 < "$$p"; \
		done; \
	fi

build:
	@echo "tmux: in-OS build path deferred — see xtask port build" && exit 1

install:
	@echo "tmux: in-OS install path deferred — see xtask port build" && exit 1

clean:
	rm -rf $(WORKDIR)
