#!/bin/sh
# First-time setup for m3OS development.
# Run once after cloning to install git hooks.

set -e

REPO_ROOT="$(cd "$(dirname "$0")" && pwd)"

echo "Installing git hooks..."
git -C "$REPO_ROOT" config core.hooksPath .githooks
echo "  core.hooksPath set to .githooks/"
echo "  pre-commit: runs cargo xtask check before each commit"
echo "  pre-push:   runs cargo xtask check, smoke-test, and regression before each push"
echo "              set M3OS_E1000_REGRESSION=1 to also run ssh-e1000-banner-check"
echo ""
echo "Done. To bypass hooks in an emergency: git commit/push --no-verify"
