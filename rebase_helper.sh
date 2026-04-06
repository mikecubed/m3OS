#!/bin/bash
set -e

# Helper to resolve conflicts and continue rebase
resolve_and_continue() {
    local changed=false
    
    # Check for kernel/Cargo.toml version conflict
    if grep -q "<<<<<<" kernel/Cargo.toml 2>/dev/null; then
        sed -i 's/^version = "0\.46\.0"$/version = "0.47.0"/' kernel/Cargo.toml
        # Remove conflict markers if still present
        python3 -c "
import re, sys
with open('kernel/Cargo.toml') as f: c = f.read()
if '<<<<<<<' in c:
    c = re.sub(r'<<<<<<< HEAD\nversion = \"0\.46\.0\"\n=======\nversion = \"0\.47\.0\"\n>>>>>>> [^\n]+\n', 'version = \"0.47.0\"\n', c)
    with open('kernel/Cargo.toml', 'w') as f: f.write(c)
    print('Fixed kernel/Cargo.toml')
"
        git add kernel/Cargo.toml
        changed=true
    fi
    
    # Check for Cargo.lock conflict - just take theirs
    if grep -q "<<<<<<" Cargo.lock 2>/dev/null; then
        git checkout --theirs Cargo.lock 2>/dev/null || true
        git add Cargo.lock 2>/dev/null || true
        changed=true
    fi
    
    # Check for add/add conflicts in userspace/doom/patches/ - take theirs
    for f in userspace/doom/patches/*.c userspace/doom/patches/*.h userspace/doom/dg_m3os.c userspace/doom/doomgeneric/doomgeneric.h; do
        if [ -f "$f" ] && grep -q "<<<<<<" "$f" 2>/dev/null; then
            git checkout --theirs "$f" 2>/dev/null || true
            git add "$f" 2>/dev/null || true
            changed=true
            echo "Resolved add/add: $f"
        fi
    done
    
    echo "done"
}

resolve_and_continue
