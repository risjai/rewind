#!/bin/bash
#
# Snapshots — Checkpoint and restore your workspace.
#
# Rewind snapshots let you save the state of your working directory before
# running an agent, then restore if something goes wrong. Like git stash,
# but for agent-modified files.
#
# Setup:
#     # Just need the rewind CLI in PATH
#     rewind snapshot   # to take a snapshot
#     rewind restore    # to go back
#
# No API keys, no mock server, no Python needed.

set -e

echo "=== Rewind Snapshots Demo ==="
echo

# Create a temporary working directory
WORK_DIR=$(mktemp -d)
echo "Working directory: $WORK_DIR"
echo

# Create some files to snapshot
echo "Step 1: Creating initial files..."
echo "Hello, World!" > "$WORK_DIR/greeting.txt"
echo "important data" > "$WORK_DIR/data.csv"
echo "  greeting.txt: $(cat "$WORK_DIR/greeting.txt")"
echo "  data.csv:     $(cat "$WORK_DIR/data.csv")"
echo

# Take a snapshot
echo "Step 2: Taking snapshot..."
SNAPSHOT_OUTPUT=$(rewind snapshot --dir "$WORK_DIR" 2>&1) || true
echo "  $SNAPSHOT_OUTPUT"
echo

# List snapshots
echo "Step 3: Listing snapshots..."
rewind snapshots 2>&1 || true
echo

# Simulate an agent modifying files (badly)
echo "Step 4: Simulating agent modifications..."
echo "CORRUPTED DATA" > "$WORK_DIR/greeting.txt"
echo "wrong,values,here" > "$WORK_DIR/data.csv"
echo "Agent has run and corrupted the files!"
echo "  greeting.txt: $(cat "$WORK_DIR/greeting.txt")"
echo "  data.csv:     $(cat "$WORK_DIR/data.csv")"
echo

# Restore from snapshot
echo "Step 5: Restoring from snapshot..."
rewind restore --dir "$WORK_DIR" 2>&1 || true
echo

# Verify restoration
echo "Step 6: Verifying restored files..."
echo "  greeting.txt: $(cat "$WORK_DIR/greeting.txt")"
echo "  data.csv:     $(cat "$WORK_DIR/data.csv")"
echo

# Cleanup
rm -rf "$WORK_DIR"
echo "Done! Files restored to their pre-agent state."
