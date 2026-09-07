#!/bin/bash
#
# Install Git Hooks for Carrick
#
# This script installs pre-commit hooks that run tests before committing.
# Run this once after cloning the repository.
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
cat > "$HOOKS_DIR/pre-commit.new" << 'EOF'
#!/bin/bash
#
# Carrick Pre-Commit Hook
# Runs formatting checks, linter, and tests before allowing a commit
#

set -euo pipefail  # Exit on error and fail pipelines

# Isolate child processes (notably Rust tests that `git init` throwaway repos in
# tempdirs) from the ambient GIT_DIR / GIT_WORK_TREE / GIT_INDEX_FILE that git
# sets for this hook. Without this a test's `git add .` targets THIS repo's
# index, and inside a linked worktree it corrupts the worktree index mid-commit
# (it produced bogus "delete the whole repo" trees during the eval work).
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_OBJECT_DIRECTORY GIT_COMMON_DIR

REPO_ROOT="$(git rev-parse --show-toplevel)"

echo "🪢 Carrick Pre-Commit Hook"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

# Check if CARRICK_API_ENDPOINT is set
if [ -z "${CARRICK_API_ENDPOINT:-}" ]; then
    echo "⚠️  CARRICK_API_ENDPOINT not set, using default for testing"
    export CARRICK_API_ENDPOINT="https://test.example.com"
fi

# Run cargo fmt check
echo ""
echo "Checking code formatting..."
echo ""

if ! cargo fmt --check; then
    echo ""
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "❌ Code formatting issues found! Commit blocked."
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo ""
    echo "Run 'cargo fmt' to fix formatting issues and try again."
    echo "To bypass this hook (not recommended): git commit --no-verify"
    echo ""
    exit 1
fi

echo "✅ Code formatting looks good!"

# Run clippy
echo ""
echo "Running clippy linter..."
echo ""

# Not captured to a file (carrick#740). A fixed path under /tmp is one file
# shared by every checkout of this repo, so two hooks running at once delete
# each other's copy mid-run and the second commit fails on a missing file
# rather than on a failing check. Nothing ever read this one back.
if ! cargo clippy --all-targets --all-features -- -D warnings 2>&1; then
    echo ""
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "❌ Clippy warnings found! Commit blocked."
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo ""
    echo "Fix the clippy warnings and try again."
    echo "To bypass this hook (not recommended): git commit --no-verify"
    echo ""
    exit 1
fi

echo "✅ Clippy checks passed!"

# Build the type sidecar first: several integration tests spawn
# src/sidecar/dist and either fail against a stale build or skip silently
# when there is none, so a green run on an old dist proves nothing.
if [ -f src/sidecar/package.json ]; then
    echo ""
    echo "Building type sidecar..."
    if ! (cd src/sidecar && npm run build --silent); then
        echo ""
        echo "❌ Sidecar build failed. Run: cd src/sidecar && npm ci && npm run build"
        exit 1
    fi
    echo "✅ Sidecar built"
fi

# Run Rust tests
echo ""
echo "Running Rust test suite..."
echo ""

# Not captured either, and for the same reason: the only thing ever read back
# out of this capture was a count nothing printed.
if cargo test --quiet 2>&1; then
    echo ""
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "✅ Rust tests passed!"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo ""
else
    echo ""
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "❌ Tests failed! Commit blocked."
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo ""
    echo "Fix the failing tests and try again."
    echo "To bypass this hook (not recommended): git commit --no-verify"
    echo ""

    exit 1
fi

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "✅ All tests passed!"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

exit 0
EOF

# Make hook executable
chmod +x "$HOOKS_DIR/pre-commit.new"
mv "$HOOKS_DIR/pre-commit.new" "$HOOKS_DIR/pre-commit"

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "✅ Git hooks installed successfully!"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""
echo "Installed hooks:"
echo "  • pre-commit - Runs formatting checks, linter, and tests before each commit"
echo ""
echo "To bypass hooks temporarily: git commit --no-verify"
echo ""
