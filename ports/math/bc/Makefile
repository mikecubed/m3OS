# bc port Makefile
# Source is pre-extracted by the ports system into src/

PORTDIR := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))
PREFIX ?= /usr/local
WORKDIR := $(PORTDIR)/work

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
	cd $(WORKDIR) && tcc -o bc bc.c

install:
	mkdir -p $(PREFIX)/bin
	cp $(WORKDIR)/bc $(PREFIX)/bin/bc

clean:
	rm -rf $(WORKDIR)
