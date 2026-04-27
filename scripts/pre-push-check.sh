#!/usr/bin/env bash
# Pre-push verification — mirrors CI exactly. Run BEFORE pushing.
#
# CI is split across two GitHub Actions workflows (.github/workflows/ci.yml):
#
#   - "build (ubuntu-latest)" / "build (macos-latest)":
#         cargo clippy -- -D warnings
#         cargo test --workspace
#
#   - "python":
#         pip install -e .                       (rewind_agent itself)
#         ruff check .                           (Python lint)
#         pip install pytest -q                  (no httpx/requests/aiohttp)
#         python -m pytest tests/ -v
#
# The "python" job runs WITHOUT the optional HTTP libraries — if your tests
# fail in CI but pass locally, it's almost always because your local env has
# httpx/requests/aiohttp installed and CI doesn't. Stage 3 below simulates
# CI's bare environment via a sys.meta_path import blocker so you catch the
# discrepancy locally before pushing.
#
# Usage:
#   ./scripts/pre-push-check.sh
#
# Exit code 0 = safe to push. Non-zero = something CI will reject.

set -e

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

echo "============================================="
echo "Pre-push verification (mirrors CI exactly)"
echo "============================================="
echo

echo "[1/5] ruff check (Python lint, mirrors CI's 'ruff check .')"
cd "$REPO_ROOT/python"
python3 -m ruff check . || {
    echo "❌ ruff failed — run 'python3 -m ruff check . --fix' to auto-fix"
    exit 1
}
echo "✅ ruff clean"
echo

echo "[2/5] pytest tests/ — local env (with httpx/requests/aiohttp)"
find . -name "__pycache__" -type d -exec rm -rf {} + 2>/dev/null || true
python3 -m pytest tests/ -q --no-header || {
    echo "❌ local pytest failed"
    exit 1
}
echo "✅ local pytest passed"
echo

echo "[3/5] pytest tests/ — simulated bare env (CI mirror)"
# Heredoc-into-python with explicit error trap (avoids `|| {}` + heredoc
# interaction which bash parses awkwardly).
python3 - <<'PYEOF'
import sys, importlib.abc, pytest, subprocess

class Blocker(importlib.abc.MetaPathFinder):
    """Hide httpx/requests/aiohttp so simulated env mirrors CI's
    'pip install pytest -q' (which doesn't install those libs).
    """
    BLOCKED = {'httpx', 'requests', 'aiohttp'}
    def find_spec(self, name, *args, **kwargs):
        if name.split('.')[0] in self.BLOCKED:
            raise ImportError(f'(simulated CI) {name} not installed')
        return None

sys.meta_path.insert(0, Blocker())

# Only run TRACKED test files; CI's checkout doesn't include local-only
# scratch (e.g. test_replay_e2e.py uses openai which CI doesn't have).
tracked = [
    t for t in subprocess.check_output(['git', 'ls-files', 'tests/']).decode().split()
    if t.endswith('.py')
]
print(f'(running {len(tracked)} tracked test files)')
exit_code = pytest.main(tracked + ['-q', '--no-header'])
sys.exit(exit_code)
PYEOF
if [ $? -ne 0 ]; then
    echo "❌ bare-env pytest failed — issue would surface in CI but not local"
    exit 1
fi
echo "✅ bare-env pytest passed"
echo

cd "$REPO_ROOT"
echo "[4/5] cargo clippy -- -D warnings (Rust CI)"
rustup run stable cargo clippy -- -D warnings || {
    echo "❌ clippy failed"
    exit 1
}
echo "✅ clippy clean"
echo

echo "[5/5] cargo test --workspace (Rust CI)"
rustup run stable cargo test --workspace || {
    echo "❌ cargo test failed"
    exit 1
}
echo "✅ cargo test passed"
echo

echo "============================================="
echo "✅ All pre-push checks passed — safe to push"
echo "============================================="
