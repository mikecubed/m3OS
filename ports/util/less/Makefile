# less port Makefile (Phase 69d Track B.1)
#
# Host-side build via `cargo xtask port build less`. In-OS build path is
# deferred until tcc can drive autoconf — same constraint as ncurses.

PORTDIR := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))
PREFIX ?= /usr/local
WORKDIR := $(PORTDIR)/work

.PHONY: fetch patch build install clean

fetch:
	@echo "less: in-OS fetch path deferred — see xtask port build" && exit 1

patch:
	@if [ -d "$(PORTDIR)/patches" ]; then \
		for p in $(PORTDIR)/patches/*.patch; do \
			[ -f "$$p" ] && patch -d $(WORKDIR) -p1 < "$$p"; \
		done; \
	fi

build:
	@echo "less: in-OS build path deferred — see xtask port build" && exit 1

install:
	@echo "less: in-OS install path deferred — see xtask port build" && exit 1

clean:
	rm -rf $(WORKDIR)
