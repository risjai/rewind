"""
OpenTelemetry trace importer for Rewind.

Imports OTLP traces (protobuf or JSON files) into a running Rewind server
via the POST /v1/traces endpoint, or directly into the local store.
"""

from __future__ import annotations

import json
import logging
from pathlib import Path
from typing import Optional

import urllib.request
import urllib.error

logger = logging.getLogger("rewind_agent")

# Default Rewind server endpoint
DEFAULT_ENDPOINT = "http://127.0.0.1:4800"


def import_otel(
    file_path: Optional[str] = None,
    json_data: Optional[dict] = None,
    session_name: Optional[str] = None,
    endpoint: str = DEFAULT_ENDPOINT,
) -> str:
    """Import an OTel trace into Rewind.

    Supports three input modes:
    1. ``file_path`` pointing to a protobuf file (ExportTraceServiceRequest)
    2. ``file_path`` pointing to a JSON file (OTLP JSON format, detected by .json extension)
    3. ``json_data`` dict (OTLP JSON format, sent directly)

    Args:
        file_path: Path to a protobuf or JSON file.
        json_data: OTLP JSON dict (alternative to file_path).
        session_name: Override the session name.
        endpoint: Rewind server URL (default: http://127.0.0.1:4800).

    Returns:
        The created session ID.

    Raises:
        ValueError: If no input is provided.
        ConnectionError: If the Rewind server is not reachable.
    """
    if file_path is not None:
        path = Path(file_path)
        data = path.read_bytes()
        is_json = path.suffix.lower() == ".json"

        if is_json:
            content_type = "application/json"
            body = data
        else:
            content_type = "application/x-protobuf"
            body = data

    elif json_data is not None:
        content_type = "application/json"
        body = json.dumps(json_data).encode()
    else:
        raise ValueError("Provide either file_path or json_data")

    url = f"{endpoint.rstrip('/')}/v1/traces"

    headers = {"Content-Type": content_type}
    if session_name:
        headers["X-Rewind-Session-Name"] = session_name

    req = urllib.request.Request(url, data=body, headers=headers, method="POST")

    try:
        with urllib.request.urlopen(req) as resp:
            _ = resp.read()
            session_id = resp.headers.get("X-Rewind-Session-Id", "")
            logger.info("Imported OTel trace via %s (session: %s)", url, session_id[:8])
            return session_id if session_id else "<unknown — check rewind sessions>"
    except urllib.error.URLError as e:
        raise ConnectionError(
            f"Failed to connect to Rewind at {endpoint}. "
            f"Is the server running? Error: {e}"
        ) from e
