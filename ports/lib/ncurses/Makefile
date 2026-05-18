# ncurses port Makefile (Phase 69d Track A.1)
#
# The host-side build is driven by `cargo xtask port build ncurses`, which
# fetches the upstream tarball, verifies SHA256, applies any patches under
# `patches/`, and cross-compiles two static-library variants (narrow + wide)
# against the m3OS musl toolchain. This Makefile documents the equivalent
# in-OS build path under the Phase 45 ports system; it is wired in once
# tcc is able to drive ncurses' autoconf-generated build (deferred — see
# `docs/appendix/tui-app-port-notes.md`).

PORTDIR := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))
PREFIX ?= /usr/local
WORKDIR := $(PORTDIR)/work

.PHONY: fetch patch build install clean

fetch:
	@echo "ncurses: in-OS fetch path deferred — see xtask port build" && exit 1

patch:
	@if [ -d "$(PORTDIR)/patches" ]; then \
		for p in $(PORTDIR)/patches/*.patch; do \
			[ -f "$$p" ] && patch -d $(WORKDIR) -p1 < "$$p"; \
		done; \
	fi

build:
	@echo "ncurses: in-OS build path deferred — see xtask port build" && exit 1

install:
	@echo "ncurses: in-OS install path deferred — see xtask port build" && exit 1

clean:
	rm -rf $(WORKDIR)
