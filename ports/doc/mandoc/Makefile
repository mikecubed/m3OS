# mandoc port Makefile
# Source is pre-extracted by the ports system into src/

PORTDIR := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))
PREFIX ?= /usr/local
WORKDIR := $(PORTDIR)/work

MANDOC_SRC := mandoc.c

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
	cd $(WORKDIR) && tcc -o mandoc $(MANDOC_SRC)

install:
	mkdir -p $(PREFIX)/bin
	cp $(WORKDIR)/mandoc $(PREFIX)/bin/mandoc

clean:
	rm -rf $(WORKDIR)
