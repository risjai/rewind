"""
Bootstrap the Rewind CLI binary.

When `pip install rewind-agent` creates the `rewind` console script,
this module handles:
  1. Checking if the native binary is already cached (~/.rewind/bin/rewind)
  2. Downloading the correct platform binary from GitHub Releases
  3. Delegating all arguments to the native binary

This follows the same pattern as ruff, uv, and deno — a Python entry
point that bootstraps a native binary on first use.

NOTE: This module intentionally avoids importing `rewind_agent` or
`urllib` at the top level.  The full package pulls in hooks.py which
imports urllib.request — a heavy stdlib chain that can hang or crash
on broken Python installations (e.g. incomplete Homebrew builds).
Instead we read __version__ from __init__.py with a regex and defer
urllib to the point where a download is actually needed.
"""

import os
import platform
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile

GITHUB_REPO = "agentoptics/rewind"
CACHE_DIR = os.path.join(os.path.expanduser("~"), ".rewind", "bin")
BINARY_NAME = "rewind"


def _read_version() -> str:
    """Read __version__ from __init__.py without importing the package."""
    init = os.path.join(os.path.dirname(__file__), "__init__.py")
    with open(init) as f:
        for line in f:
            m = re.match(r'^__version__\s*=\s*["\']([^"\']+)["\']', line)
            if m:
                return m.group(1)
    raise RuntimeError("Cannot find __version__ in __init__.py")


__version__ = _read_version()


def _get_platform_key() -> str:
    """Map the current OS/arch to the GitHub Release asset name suffix."""
    system = platform.system().lower()   # 'darwin', 'linux'
    machine = platform.machine().lower() # 'x86_64', 'arm64', 'aarch64'

    if system == "darwin":
        os_key = "darwin"
    elif system == "linux":
        os_key = "linux"
    else:
        _die(f"Unsupported OS: {system}. Rewind supports macOS and Linux.")
        return ""  # unreachable

    if machine in ("x86_64", "amd64"):
        arch_key = "x86_64"
    elif machine in ("arm64", "aarch64"):
        arch_key = "aarch64"
    else:
        _die(f"Unsupported architecture: {machine}. Rewind supports x86_64 and aarch64.")
        return ""  # unreachable

    return f"{os_key}-{arch_key}"


def _binary_path() -> str:
    """Path where the cached binary lives, versioned to avoid stale binaries."""
    return os.path.join(CACHE_DIR, f"rewind-{__version__}")


def _download_url(tag: str, platform_key: str) -> str:
    """GitHub Release download URL for a specific version and platform."""
    asset = f"rewind-{tag}-{platform_key}.tar.gz"
    return f"https://github.com/{GITHUB_REPO}/releases/download/{tag}/{asset}"


def _die(msg: str):
    print(f"\033[31mrewind: {msg}\033[0m", file=sys.stderr)
    sys.exit(1)


def _ensure_binary() -> str:
    """Return path to the rewind binary, downloading if necessary."""
    bin_path = _binary_path()

    if os.path.isfile(bin_path) and os.access(bin_path, os.X_OK):
        return bin_path

    # Check if a locally-built binary exists (dev mode)
    local_paths = [
        os.path.join(os.path.dirname(__file__), "..", "..", "target", "release", BINARY_NAME),
        shutil.which(BINARY_NAME) or "",
    ]
    for p in local_paths:
        if p and os.path.isfile(p) and os.access(p, os.X_OK):
            return p

    # Download from GitHub Releases — import urllib lazily
    import urllib.request
    import urllib.error

    platform_key = _get_platform_key()
    tag = f"v{__version__}"
    url = _download_url(tag, platform_key)

    print()
    print(f"  \033[36m\033[1m  ⏪  r e w i n d\033[0m")
    print(f"  \033[2m  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\033[0m")
    print(f"  \033[2m  The time-travel debugger for AI agents\033[0m")
    print()
    print(f"  Downloading CLI {tag} for {platform_key}...")
    print(f"  \033[2m{url}\033[0m")
    print()

    os.makedirs(CACHE_DIR, exist_ok=True)

    try:
        with tempfile.NamedTemporaryFile(suffix=".tar.gz", delete=False) as tmp:
            tmp_path = tmp.name
            urllib.request.urlretrieve(url, tmp_path)

        # Extract the binary from the tarball
        with tarfile.open(tmp_path, "r:gz") as tar:
            # The tarball contains a single file named "rewind"
            members = tar.getmembers()
            rewind_member = None
            for m in members:
                if m.name == BINARY_NAME or m.name.endswith(f"/{BINARY_NAME}"):
                    rewind_member = m
                    break

            if rewind_member is None:
                _die(f"Binary not found in release archive. Contents: {[m.name for m in members]}")

            with tempfile.TemporaryDirectory() as extract_dir:
                tar.extract(rewind_member, path=extract_dir)
                extracted = os.path.join(extract_dir, rewind_member.name)
                shutil.move(extracted, bin_path)

        # Make executable
        os.chmod(bin_path, os.stat(bin_path).st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)

    except urllib.error.HTTPError as e:
        if e.code == 404:
            _die(
                f"Release {tag} not found for {platform_key}.\n"
                f"  URL: {url}\n"
                f"  Install from source instead:\n"
                f"    cargo install --git https://github.com/{GITHUB_REPO} rewind-cli"
            )
        _die(f"Download failed: {e}")
    except Exception as e:
        _die(f"Download failed: {e}")
    finally:
        if os.path.exists(tmp_path):
            os.unlink(tmp_path)

    print(f"  \033[32m✓\033[0m Installed rewind {tag} to {bin_path}")
    print(f"  \033[2mRun \033[0m\033[32mrewind demo\033[0m\033[2m to try it out\033[0m")
    print()

    return bin_path


def main():
    """Entry point for the `rewind` console script."""
    binary = _ensure_binary()

    # Delegate everything to the native binary
    try:
        result = subprocess.run([binary] + sys.argv[1:])
        sys.exit(result.returncode)
    except KeyboardInterrupt:
        sys.exit(130)
    except FileNotFoundError:
        _die(f"Binary not found at {binary}. Try: pip install --force-reinstall rewind-agent")


if __name__ == "__main__":
    main()
