# minizip port Makefile
# A simple zip utility that depends on zlib to validate dependency resolution

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
	cd $(WORKDIR) && tcc -o minizip minizip.c -I$(PREFIX)/include -L$(PREFIX)/lib -lz

install:
	mkdir -p $(PREFIX)/bin
	cp $(WORKDIR)/minizip $(PREFIX)/bin/minizip

clean:
	rm -rf $(WORKDIR)
