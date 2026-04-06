# zlib port Makefile
# Source is pre-extracted by the ports system into src/

PORTDIR := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))
PREFIX ?= /usr/local
WORKDIR := $(PORTDIR)/work

ZLIB_SRC := adler32.c compress.c crc32.c deflate.c gzclose.c gzlib.c \
            gzread.c gzwrite.c infback.c inffast.c inflate.c inftrees.c \
            trees.c uncompr.c zutil.c

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
	cd $(WORKDIR) && for f in $(ZLIB_SRC); do tcc -c $$f; done && \
	tcc -ar rcs libz.a adler32.o compress.o crc32.o deflate.o gzclose.o \
		gzlib.o gzread.o gzwrite.o infback.o inffast.o inflate.o \
		inftrees.o trees.o uncompr.o zutil.o

install:
	mkdir -p $(PREFIX)/lib $(PREFIX)/include
	cp $(WORKDIR)/libz.a $(PREFIX)/lib/
	cp $(WORKDIR)/zlib.h $(WORKDIR)/zconf.h $(PREFIX)/include/

clean:
	rm -rf $(WORKDIR)
