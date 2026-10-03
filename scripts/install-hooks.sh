#!/bin/bash
#
# Install Git Hooks for Carrick
#
# Copies the tracked hook in scripts/hooks/ into this clone's hooks directory.
# Run it once after cloning, and again after a change to scripts/hooks/.
#

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"

echo "🪢 Installing Carrick Git Hooks"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

# Asked of git rather than assembled from the repo root, because inside a
# linked worktree `.git` is a FILE pointing at the real directory and this
# script simply refused to run there — which is the checkout the hook's own
# parallel-run problem shows up in (carrick#740). The COMMON dir is deliberate:
# hooks live once per repository and every worktree runs the same file.
if ! HOOKS_DIR="$(cd "$REPO_ROOT" && git rev-parse --git-common-dir)/hooks"; then
    echo "❌ Error: not a git repository."
    exit 1
fi
case "$HOOKS_DIR" in
    /*) ;;
    *) HOOKS_DIR="$REPO_ROOT/$HOOKS_DIR" ;;
esac

# Create hooks directory if it doesn't exist
mkdir -p "$HOOKS_DIR"

# Install pre-commit hook
echo "Installing pre-commit hook..."

# Written beside the hook and moved into place, never truncated in place: the
# hooks directory is shared by every worktree of this repo, so a re-install can
# land while another checkout's commit is part-way through reading its own copy
# of the hook (carrick#740).
cp "$SCRIPT_DIR/hooks/pre-commit" "$HOOKS_DIR/pre-commit.new"
chmod +x "$HOOKS_DIR/pre-commit.new"
mv "$HOOKS_DIR/pre-commit.new" "$HOOKS_DIR/pre-commit"

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "✅ Git hooks installed successfully!"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""
echo "Installed hooks:"
echo "  • pre-commit - Runs formatting checks, linter, and tests before each commit"
echo "    (installs src/sidecar dependencies itself on a checkout's first commit)"
echo ""
echo "To bypass hooks temporarily: git commit --no-verify"
echo ""
