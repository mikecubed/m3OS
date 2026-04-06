# sbase port Makefile
# Source is pre-extracted by the ports system into src/
# Builds multiple sbase utilities from individual source files

PORTDIR := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))
PREFIX ?= /usr/local
WORKDIR := $(PORTDIR)/work

# sbase utilities to build (each has a corresponding .c file)
SBASE_UTILS := basename cal cols comm dirname expand fold join nl \
               paste printenv rev seq sponge tac unexpand

.PHONY: fetch patch build install clean

fetch:
	mkdir -p $(WORKDIR)
	cp -r $(PORTDIR)/src/* $(WORKDIR)/

patch:
	@if [ -d "$(PORTDIR)/patches" ]; then \
		for p in $(PORTDIR)/patches/*.patch; do \
			[ -f "$$p" ] && patch -d $(WORKDIR) -p1 < "$$p"; \
		done; \
	fi

build:
	@for util in $(SBASE_UTILS); do \
		echo "Building $$util..."; \
		cd $(WORKDIR) && tcc -o $$util $$util.c || true; \
	done

install:
	mkdir -p $(PREFIX)/bin
	@for util in $(SBASE_UTILS); do \
		if [ -f "$(WORKDIR)/$$util" ]; then \
			cp $(WORKDIR)/$$util $(PREFIX)/bin/$$util; \
		fi; \
	done

clean:
	rm -rf $(WORKDIR)
