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
HOOKS_DIR="$REPO_ROOT/.git/hooks"

echo "🪢 Installing Carrick Git Hooks"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

# Check if .git directory exists
if [ ! -d "$REPO_ROOT/.git" ]; then
    echo "❌ Error: .git directory not found. Are you in a git repository?"
    exit 1
fi

# Create hooks directory if it doesn't exist
mkdir -p "$HOOKS_DIR"

# Install pre-commit hook
echo "Installing pre-commit hook..."

cat > "$HOOKS_DIR/pre-commit" << 'EOF'
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

if ! cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tee /tmp/carrick-clippy-output.txt; then
    echo ""
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "❌ Clippy warnings found! Commit blocked."
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo ""
    echo "Fix the clippy warnings and try again."
    echo "To bypass this hook (not recommended): git commit --no-verify"
    echo ""
    rm -f /tmp/carrick-clippy-output.txt
    exit 1
fi

rm -f /tmp/carrick-clippy-output.txt
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

if cargo test --quiet 2>&1 | tee /tmp/carrick-test-output.txt; then
    # Count test results
    PASSED=$(grep -o "test result: ok" /tmp/carrick-test-output.txt | wc -l | tr -d ' ')

    echo ""
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "✅ Rust tests passed!"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo ""

    # Clean up
    rm -f /tmp/carrick-test-output.txt

else
    echo ""
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "❌ Tests failed! Commit blocked."
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo ""
    echo "Fix the failing tests and try again."
    echo "To bypass this hook (not recommended): git commit --no-verify"
    echo ""

    # Clean up
    rm -f /tmp/carrick-test-output.txt

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
chmod +x "$HOOKS_DIR/pre-commit"

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
