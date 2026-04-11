#!/usr/bin/env python3
"""Build a properly-timed asciinema cast file from rewind CLI output.

Runs the commands, captures output, and writes a .cast file with
smooth timing so the GIF looks natural.
"""
import json
import subprocess
import sys
import os

REWIND = "./target/release/rewind"
COLS = 100
ROWS = 40

def run(cmd: str) -> str:
    """Run a command and return its output."""
    result = subprocess.run(cmd, shell=True, capture_output=True, text=True, env={**os.environ, "NO_COLOR": ""})
    return result.stdout + result.stderr

def make_cast(output_path: str):
    # Get session info for diff command
    sid = run(f"{REWIND} query \"SELECT id FROM sessions ORDER BY created_at DESC LIMIT 1\"").strip().split('\n')[2].strip()
    main_tid = run(f"{REWIND} query \"SELECT id FROM timelines WHERE session_id='{sid}' AND label='main'\"").strip().split('\n')[2].strip()
    fixed_tid = run(f"{REWIND} query \"SELECT id FROM timelines WHERE session_id='{sid}' AND label='fixed'\"").strip().split('\n')[2].strip()

    # Capture outputs with forced color (the colored crate checks CLICOLOR_FORCE)
    env_color = {**os.environ}
    env_color.pop("NO_COLOR", None)
    env_color["CLICOLOR_FORCE"] = "1"

    show_out = subprocess.run(f"{REWIND} show latest", shell=True, capture_output=True, text=True, env=env_color).stdout
    diff_out = subprocess.run(f"{REWIND} diff {sid} {main_tid} {fixed_tid}", shell=True, capture_output=True, text=True, env=env_color).stdout
    assert_out = subprocess.run(f"{REWIND} assert check latest --against demo-baseline", shell=True, capture_output=True, text=True, env=env_color).stdout

    events = []
    t = 0.0

    def add_typing(text: str, delay_per_char: float = 0.04):
        """Simulate typing a command."""
        nonlocal t
        for ch in text:
            events.append([round(t, 4), "o", ch])
            t += delay_per_char

    def add_output(text: str, line_delay: float = 0.08):
        """Add output lines with natural pacing."""
        nonlocal t
        for line in text.split('\n'):
            events.append([round(t, 4), "o", line + "\r\n"])
            t += line_delay

    def add_pause(seconds: float):
        nonlocal t
        t += seconds

    # ── Command 1: rewind show latest ──
    # Prompt
    events.append([round(t, 4), "o", "\x1b[1;36m❯\x1b[0m "])
    t += 0.2
    add_typing("rewind show latest")
    add_pause(0.3)
    events.append([round(t, 4), "o", "\r\n"])
    t += 0.4

    # Output
    add_output(show_out, line_delay=0.12)
    add_pause(3.0)

    # ── Command 2: rewind diff ──
    events.append([round(t, 4), "o", "\x1b[1;36m❯\x1b[0m "])
    t += 0.2
    add_typing("rewind diff latest main fixed")
    add_pause(0.3)
    events.append([round(t, 4), "o", "\r\n"])
    t += 0.4

    add_output(diff_out, line_delay=0.15)
    add_pause(2.5)

    # ── Command 3: rewind assert check ──
    events.append([round(t, 4), "o", "\x1b[1;36m❯\x1b[0m "])
    t += 0.2
    add_typing("rewind assert check latest --against demo-baseline")
    add_pause(0.3)
    events.append([round(t, 4), "o", "\r\n"])
    t += 0.4

    add_output(assert_out, line_delay=0.1)
    add_pause(2.0)

    # Write cast file
    header = {
        "version": 2,
        "width": COLS,
        "height": ROWS,
        "env": {"SHELL": "/bin/zsh", "TERM": "xterm-256color"},
    }

    with open(output_path, "w") as f:
        f.write(json.dumps(header) + "\n")
        for event in events:
            f.write(json.dumps(event) + "\n")

    print(f"Written {len(events)} events, {round(t, 1)}s total to {output_path}")

if __name__ == "__main__":
    make_cast(sys.argv[1] if len(sys.argv) > 1 else "assets/demo-v2.cast")
