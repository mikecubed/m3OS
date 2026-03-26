#!/bin/sh
# First-time setup for m3OS development.
# Run once after cloning to install git hooks.

set -e

REPO_ROOT="$(cd "$(dirname "$0")" && pwd)"

echo "Installing git hooks..."
git -C "$REPO_ROOT" config core.hooksPath .githooks
echo "  core.hooksPath set to .githooks/"
echo "  pre-commit: runs cargo xtask check before each commit"
echo "  pre-push:   runs cargo xtask check before each push"
echo ""
echo "Done. To bypass hooks in an emergency: git commit/push --no-verify"
