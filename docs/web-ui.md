# Web UI -- Browser-Based Dashboard

Rewind is a time-travel debugger for AI agents that records every LLM call for inspection, forking, replay, and diffing. The web UI provides a browser-based dashboard to explore recorded sessions, inspect context windows, diff timelines, and watch live recordings.

<p align="center">
  <img src="../assets/web-ui-screenshot.png" alt="Rewind Web UI — session timeline, step detail with context window" width="800" />
</p>

## Getting Started

Run `rewind web` and open `http://localhost:8080`:

```bash
rewind web [--port 8080]
```

To start recording with the live web dashboard:

```bash
rewind record --web
```

## Key Features

- **Session explorer** -- Browse all recorded sessions with stats
- **Step timeline** -- Walk through each step with status icons, timing, and token counts
- **Context window viewer** -- See the exact context window at each step: every message, system prompt, and tool response the model saw
- **Timeline diff** -- Compare two timelines side by side to see where they diverge
- **WebSocket live** -- Watch recordings in real-time as your agent runs via WebSocket streaming

## Everything Embedded in a Single Binary

The entire web UI is embedded in the single Rewind binary -- no Docker, no Node.js runtime needed. Just run `rewind web` and everything works out of the box.
