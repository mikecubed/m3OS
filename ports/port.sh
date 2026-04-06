#!/bin/sh
# port - BSD-style ports package manager for m3OS
#
# Usage: port <command> [args]
# Commands: list, info, install, remove, clean
#
# The ports tree lives at /usr/ports/category/program/ and each port
# contains a Portfile (shell variables) and a Makefile with standard
# targets: fetch, patch, build, install, clean.
#
# Package database: /var/db/ports/installed (flat file)
# Install manifests: /var/db/ports/<name>.manifest (one path per line)
# Install prefix: /usr/local (bin, lib, include)

PORTS_DIR="/usr/ports"
DB_DIR="/var/db/ports"
INSTALLED_DB="$DB_DIR/installed"
PREFIX="/usr/local"

# --- Utility functions ---

# Print usage information and exit
usage() {
    echo "port - BSD-style ports package manager for m3OS"
    echo ""
    echo "Usage: port <command> [name]"
    echo ""
    echo "Commands:"
    echo "  list              List all available ports"
    echo "  info <name>       Show port information"
    echo "  install <name>    Build and install a port"
    echo "  remove <name>     Remove an installed port"
    echo "  clean [name]      Clean build artifacts (all ports if no name)"
    exit 1
}

# Find the port directory for a given port name.
# Searches all categories under $PORTS_DIR.
# Prints the path to the port directory, or nothing if not found.
find_port() {
    _name="$1"
    if [ -z "$_name" ]; then
        return 1
    fi
    _found=""
    for _pf in $(find $PORTS_DIR -name Portfile -type f 2>/dev/null); do
        _dir=$(dirname "$_pf")
        _base=$(basename "$_dir")
        if [ "$_base" = "$_name" ]; then
            _found="$_dir"
            break
        fi
    done
    if [ -n "$_found" ]; then
        echo "$_found"
        return 0
    fi
    return 1
}

# Check if a port is currently installed.
# Returns 0 if installed, 1 if not.
is_installed() {
    _name="$1"
    if [ -f "$INSTALLED_DB" ]; then
        grep -q "^$_name " "$INSTALLED_DB" 2>/dev/null
        return $?
    fi
    return 1
}

# Ensure the package database directory exists.
ensure_db() {
    if [ ! -d "$DB_DIR" ]; then
        mkdir -p "$DB_DIR"
    fi
}

# --- Subcommands ---

# list: enumerate all available ports
cmd_list() {
    echo "Available ports:"
    echo "---"
    for _pf in $(find $PORTS_DIR -name Portfile -type f 2>/dev/null | sort); do
        _portdir=$(dirname "$_pf")
        # Source the Portfile to read variables
        NAME=""
        VERSION=""
        DESCRIPTION=""
        CATEGORY=""
        . "$_pf"
        # Show installed status
        if is_installed "$NAME"; then
            _status="[installed]"
        else
            _status=""
        fi
        printf "  %-15s %-10s %s %s\n" "$NAME" "$VERSION" "$DESCRIPTION" "$_status"
    done
}

# info: show detailed information about a port
cmd_info() {
    _name="$1"
    if [ -z "$_name" ]; then
        echo "Error: port name required"
        echo "Usage: port info <name>"
        exit 1
    fi

    _portdir=$(find_port "$_name")
    if [ -z "$_portdir" ]; then
        echo "Error: port '$_name' not found"
        exit 1
    fi

    # Source the Portfile
    NAME=""
    VERSION=""
    DESCRIPTION=""
    CATEGORY=""
    DEPS=""
    URL=""
    SHA256=""
    MAINTAINER=""
    . "$_portdir/Portfile"

    echo "Port:        $NAME"
    echo "Version:     $VERSION"
    echo "Description: $DESCRIPTION"
    echo "Category:    $CATEGORY"
    echo "Directory:   $_portdir"
    if [ -n "$DEPS" ]; then
        echo "Depends on:  $DEPS"
    else
        echo "Depends on:  (none)"
    fi
    if [ -n "$URL" ]; then
        echo "Source URL:  $URL"
    fi
    if [ -n "$MAINTAINER" ]; then
        echo "Maintainer:  $MAINTAINER"
    fi
    if is_installed "$NAME"; then
        echo "Status:      installed"
    else
        echo "Status:      not installed"
    fi
}

# resolve_deps: install any missing dependencies for a port
resolve_deps() {
    _name="$1"
    _portdir=$(find_port "$_name")
    if [ -z "$_portdir" ]; then
        return 1
    fi

    # Source Portfile to get DEPS
    DEPS=""
    . "$_portdir/Portfile"

    if [ -n "$DEPS" ]; then
        for _dep in $DEPS; do
            if ! is_installed "$_dep"; then
                echo ">>> Installing dependency: $_dep"
                cmd_install "$_dep"
                if [ $? -ne 0 ]; then
                    echo "Error: failed to install dependency '$_dep'"
                    exit 1
                fi
            else
                echo ">>> Dependency '$_dep' already installed"
            fi
        done
    fi
}

# install: build and install a port
cmd_install() {
    _name="$1"
    if [ -z "$_name" ]; then
        echo "Error: port name required"
        echo "Usage: port install <name>"
        exit 1
    fi

    # Check if already installed
    if is_installed "$_name"; then
        echo "$_name is already installed"
        return 0
    fi

    # Find the port
    _portdir=$(find_port "$_name")
    if [ -z "$_portdir" ]; then
        echo "Error: port '$_name' not found"
        exit 1
    fi

    # Source the Portfile
    NAME=""
    VERSION=""
    DESCRIPTION=""
    DEPS=""
    . "$_portdir/Portfile"

    # Resolve dependencies first
    resolve_deps "$_name"

    echo "==> Installing $NAME $VERSION"

    # Ensure install directories exist
    mkdir -p "$PREFIX/bin" "$PREFIX/lib" "$PREFIX/include"
    ensure_db

    # Snapshot files before install (for manifest generation)
    find $PREFIX -type f > /tmp/port_before 2>/dev/null

    # Run the build lifecycle
    echo "==> Fetching source..."
    cd "$_portdir"
    make fetch
    if [ $? -ne 0 ]; then
        echo "Error: fetch failed for $NAME"
        exit 1
    fi

    echo "==> Applying patches..."
    make patch
    if [ $? -ne 0 ]; then
        echo "Error: patch failed for $NAME"
        exit 1
    fi

    echo "==> Building..."
    make build
    if [ $? -ne 0 ]; then
        echo "Error: build failed for $NAME"
        exit 1
    fi

    echo "==> Installing files..."
    make install PREFIX=$PREFIX
    if [ $? -ne 0 ]; then
        echo "Error: install failed for $NAME"
        exit 1
    fi

    # Generate manifest: find new files added during install
    find $PREFIX -type f > /tmp/port_after 2>/dev/null
    generate_manifest "$NAME"

    # Record in installed database
    track_install "$NAME" "$VERSION"

    echo "==> $NAME $VERSION installed successfully"
}

# remove: uninstall a port
cmd_remove() {
    _name="$1"
    if [ -z "$_name" ]; then
        echo "Error: port name required"
        echo "Usage: port remove <name>"
        exit 1
    fi

    # Check if installed
    if ! is_installed "$_name"; then
        echo "Error: '$_name' is not installed"
        exit 1
    fi

    # Check if other ports depend on this one
    _dependents=""
    if [ -f "$INSTALLED_DB" ]; then
        while read _line; do
            _iname=$(echo "$_line" | cut -d' ' -f1)
            if [ "$_iname" = "$_name" ]; then
                continue
            fi
            _idir=$(find_port "$_iname")
            if [ -n "$_idir" ]; then
                DEPS=""
                . "$_idir/Portfile"
                for _d in $DEPS; do
                    if [ "$_d" = "$_name" ]; then
                        _dependents="$_dependents $_iname"
                    fi
                done
            fi
        done < "$INSTALLED_DB"
    fi
    if [ -n "$_dependents" ]; then
        echo "Warning: the following installed ports depend on $_name:$_dependents"
        echo "Proceeding with removal anyway..."
    fi

    echo "==> Removing $_name..."

    # Read manifest and delete files
    _manifest="$DB_DIR/$_name.manifest"
    if [ -f "$_manifest" ]; then
        while read _file; do
            if [ -f "$_file" ]; then
                rm "$_file"
                echo "  removed $_file"
            fi
        done < "$_manifest"
        rm "$_manifest"
    else
        echo "Warning: no manifest found for $_name, cannot remove files"
    fi

    # Remove from installed database
    if [ -f "$INSTALLED_DB" ]; then
        grep -v "^$_name " "$INSTALLED_DB" > "$INSTALLED_DB.tmp" 2>/dev/null
        mv "$INSTALLED_DB.tmp" "$INSTALLED_DB"
    fi

    echo "==> $_name removed"
}

# clean: remove build artifacts
cmd_clean() {
    _name="$1"

    if [ -z "$_name" ]; then
        # Clean all ports
        echo "Cleaning all ports..."
        for _pf in $(find $PORTS_DIR -name Portfile -type f 2>/dev/null); do
            _portdir=$(dirname "$_pf")
            NAME=""
            . "$_pf"
            if [ -d "$_portdir/work" ]; then
                echo "  cleaning $NAME..."
                cd "$_portdir"
                make clean
            fi
        done
        echo "All ports cleaned"
    else
        # Clean a specific port
        _portdir=$(find_port "$_name")
        if [ -z "$_portdir" ]; then
            echo "Error: port '$_name' not found"
            exit 1
        fi

        echo "Cleaning $_name..."
        cd "$_portdir"
        make clean
        if [ -d "$_portdir/work" ]; then
            rm -rf "$_portdir/work"
        fi
        echo "$_name cleaned"
    fi
}

# generate_manifest: compare before/after file lists to find installed files
generate_manifest() {
    _name="$1"
    _manifest="$DB_DIR/$_name.manifest"

    # Compare before and after snapshots
    # Files in "after" but not in "before" are newly installed
    if [ -f /tmp/port_before ] && [ -f /tmp/port_after ]; then
        sort /tmp/port_before > /tmp/port_before_sorted
        sort /tmp/port_after > /tmp/port_after_sorted
        # Find lines only in the after list (newly installed files)
        _new_files=""
        while read _file; do
            if ! grep -q "^${_file}$" /tmp/port_before_sorted 2>/dev/null; then
                _new_files="$_new_files
$_file"
            fi
        done < /tmp/port_after_sorted
        echo "$_new_files" | sed '/^$/d' > "$_manifest"
        rm -f /tmp/port_before /tmp/port_after /tmp/port_before_sorted /tmp/port_after_sorted
    else
        # Fallback: empty manifest
        echo "" > "$_manifest"
    fi
}

# track_install: record a port as installed
track_install() {
    _name="$1"
    _version="$2"
    _date=$(date 2>/dev/null)
    if [ -z "$_date" ]; then
        _date="unknown"
    fi
    echo "$_name $_version $_date" >> "$INSTALLED_DB"
}

# --- Main dispatch ---

main() {
    if [ $# -eq 0 ]; then
        usage
    fi

    _cmd="$1"
    shift

    case "$_cmd" in
        list)
            cmd_list
            ;;
        info)
            cmd_info "$1"
            ;;
        install)
            cmd_install "$1"
            ;;
        remove)
            cmd_remove "$1"
            ;;
        clean)
            cmd_clean "$1"
            ;;
        -h|--help|help)
            usage
            ;;
        *)
            echo "Error: unknown command '$_cmd'"
            echo ""
            usage
            ;;
    esac
}

main "$@"
