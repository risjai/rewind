"""
Bootstrap the Rewind MCP server binary.

When `pip install rewind-mcp` creates the `rewind-mcp` console script,
this module handles:
  1. Checking if the native binary is already cached (~/.rewind/bin/rewind-mcp)
  2. Downloading the correct platform binary from GitHub Releases
  3. Delegating all arguments to the native binary

This follows the same pattern as the `rewind` CLI bootstrapper in
the `rewind-agent` package.

NOTE: This is a standalone module. It must NOT import from rewind_agent
or any other package beyond the standard library.
"""

import os
import platform
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile

GITHUB_REPO = "agentoptics/rewind"
CACHE_DIR = os.path.join(os.path.expanduser("~"), ".rewind", "bin")
BINARY_NAME = "rewind-mcp"

# Must match the Rust workspace version in Cargo.toml.
# Update this when a new GitHub Release is published with new binaries.
CLI_VERSION = "0.12.14"


def _get_platform_key() -> str:
    """Map the current OS/arch to the GitHub Release asset name suffix."""
    system = platform.system().lower()
    machine = platform.machine().lower()

    if system == "darwin":
        os_key = "darwin"
    elif system == "linux":
        os_key = "linux"
    else:
        _die(f"Unsupported OS: {system}. Rewind supports macOS and Linux.")
        return ""

    if machine in ("x86_64", "amd64"):
        arch_key = "x86_64"
    elif machine in ("arm64", "aarch64"):
        arch_key = "aarch64"
    else:
        _die(f"Unsupported architecture: {machine}. Rewind supports x86_64 and aarch64.")
        return ""

    return f"{os_key}-{arch_key}"


def _binary_path() -> str:
    """Path where the cached binary lives, versioned to avoid stale binaries."""
    return os.path.join(CACHE_DIR, f"rewind-mcp-{CLI_VERSION}")


def _download_url(tag: str, platform_key: str) -> str:
    """GitHub Release download URL for a specific version and platform."""
    asset = f"rewind-{tag}-{platform_key}.tar.gz"
    return f"https://github.com/{GITHUB_REPO}/releases/download/{tag}/{asset}"


def _die(msg: str):
    print(f"\033[31mrewind-mcp: {msg}\033[0m", file=sys.stderr)
    sys.exit(1)


def _ensure_binary() -> str:
    """Return path to the rewind-mcp binary, downloading if necessary."""
    bin_path = _binary_path()

    if os.path.isfile(bin_path) and os.access(bin_path, os.X_OK):
        return bin_path

    # Check if a locally-built native binary exists (dev mode).
    local_paths = [
        os.path.join(os.path.dirname(__file__), "..", "target", "release", BINARY_NAME),
        shutil.which(BINARY_NAME) or "",
    ]
    for p in local_paths:
        if p and os.path.isfile(p) and os.access(p, os.X_OK):
            try:
                with open(p, "rb") as f:
                    header = f.read(4)
                if header in (b"\xcf\xfa\xed\xfe", b"\xce\xfa\xed\xfe", b"\x7fELF"):
                    return p
            except OSError:
                continue

    import urllib.request
    import urllib.error

    platform_key = _get_platform_key()
    tag = f"v{CLI_VERSION}"
    url = _download_url(tag, platform_key)

    print(file=sys.stderr)
    print("  \033[36m\033[1m  ⏪  r e w i n d  MCP\033[0m", file=sys.stderr)
    print("  \033[2m  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\033[0m", file=sys.stderr)
    print("  \033[2m  MCP server for AI assistant integration\033[0m", file=sys.stderr)
    print(file=sys.stderr)
    print(f"  Downloading MCP server {tag} for {platform_key}...", file=sys.stderr)
    print(f"  \033[2m{url}\033[0m", file=sys.stderr)
    print(file=sys.stderr)

    os.makedirs(CACHE_DIR, exist_ok=True)

    tmp_path = None
    try:
        with tempfile.NamedTemporaryFile(suffix=".tar.gz", delete=False) as tmp:
            tmp_path = tmp.name
            urllib.request.urlretrieve(url, tmp_path)

        with tarfile.open(tmp_path, "r:gz") as tar:
            members = tar.getmembers()
            mcp_member = None
            for m in members:
                if m.name == BINARY_NAME or m.name.endswith(f"/{BINARY_NAME}"):
                    mcp_member = m
                    break

            if mcp_member is None:
                _die(
                    f"Binary '{BINARY_NAME}' not found in release archive.\n"
                    f"  Archive contents: {[m.name for m in members]}\n"
                    f"  Older releases may not include the MCP server binary.\n"
                    f"  Install from source instead:\n"
                    f"    cargo install --git https://github.com/{GITHUB_REPO} rewind-mcp"
                )

            if os.path.isabs(mcp_member.name) or ".." in mcp_member.name.split("/"):
                _die(f"Refusing to extract archive entry with path traversal: {mcp_member.name}")

            with tempfile.TemporaryDirectory() as extract_dir:
                tar.extract(mcp_member, path=extract_dir, set_attrs=False)
                extracted = os.path.join(extract_dir, mcp_member.name)
                shutil.move(extracted, bin_path)

        os.chmod(bin_path, os.stat(bin_path).st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)

    except urllib.error.HTTPError as e:
        if e.code == 404:
            _die(
                f"Release {tag} not found for {platform_key}.\n"
                f"  URL: {url}\n"
                f"  Install from source instead:\n"
                f"    cargo install --git https://github.com/{GITHUB_REPO} rewind-mcp"
            )
        _die(f"Download failed: {e}")
    except urllib.error.URLError as e:
        reason = str(getattr(e, "reason", e))
        if "CERTIFICATE_VERIFY_FAILED" in reason:
            _die(
                f"Download failed: {e}\n\n"
                f"  This is a macOS Python SSL issue, not a Rewind bug.\n"
                f"  Fix it by running one of:\n\n"
                f'    open "/Applications/Python 3.{sys.version_info.minor}/Install Certificates.command"\n'
                f"    pip install certifi\n\n"
                f"  Then retry your command."
            )
        _die(f"Download failed: {e}")
    except Exception as e:
        _die(f"Download failed: {e}")
    finally:
        if tmp_path and os.path.exists(tmp_path):
            os.unlink(tmp_path)

    print(f"  \033[32m✓\033[0m Installed rewind-mcp {tag} to {bin_path}", file=sys.stderr)
    print(file=sys.stderr)

    return bin_path


def main():
    """Entry point for the `rewind-mcp` console script."""
    binary = _ensure_binary()

    try:
        result = subprocess.run([binary] + sys.argv[1:])
        sys.exit(result.returncode)
    except KeyboardInterrupt:
        sys.exit(130)
    except FileNotFoundError:
        _die(f"Binary not found at {binary}. Try: pip install --force-reinstall rewind-mcp")


if __name__ == "__main__":
    main()
