#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/../python"

echo "Cleaning old dist artifacts..."
rm -rf dist/

echo "Building..."
python3 -m build

echo "Uploading to PyPI..."
twine upload dist/*

echo "Done."
