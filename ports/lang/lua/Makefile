# Lua port Makefile
# Source is pre-extracted by the ports system into src/

PORTDIR := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))
PREFIX ?= /usr/local
WORKDIR := $(PORTDIR)/work

# Lua source files for compilation
LUA_CORE := lapi.c lcode.c lctype.c ldebug.c ldo.c ldump.c lfunc.c lgc.c \
            llex.c lmem.c lobject.c lopcodes.c lparser.c lstate.c lstring.c \
            ltable.c ltm.c lundump.c lvm.c lzio.c
LUA_LIB := lauxlib.c lbaselib.c lcorolib.c ldblib.c liolib.c lmathlib.c \
           loadlib.c loslib.c lstrlib.c ltablib.c lutf8lib.c linit.c
LUA_MAIN := lua.c

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
	cd $(WORKDIR) && tcc -o lua -DLUA_USE_POSIX $(LUA_CORE) $(LUA_LIB) $(LUA_MAIN)

install:
	mkdir -p $(PREFIX)/bin
	cp $(WORKDIR)/lua $(PREFIX)/bin/lua

clean:
	rm -rf $(WORKDIR)
