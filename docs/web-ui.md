# Web UI -- Browser-Based Dashboard

Rewind is a time-travel debugger for AI agents that records every LLM call for inspection, forking, replay, and diffing. The web UI provides a browser-based dashboard to explore recorded sessions, visualize agent activity across time, inspect context windows, diff timelines, and watch live recordings.

<p align="center">
  <img src="../assets/web-ui-screenshot.png" alt="Rewind Web UI — activity timeline with swim lanes, step detail with context window" width="800" />
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

- **Activity Timeline** -- Horizontal swim-lane visualization where each agent or tool type gets its own lane. Steps rendered as duration bars showing relative timing. Zoom, pan, keyboard navigation (j/k/+/-/h/l), and per-lane analytics on click.
- **Timeline / List toggle** -- Switch between the visual activity timeline and the classic step-by-step list view
- **Multi-metric axis** -- Toggle bar widths between duration, token count, and estimated cost
- **Session explorer** -- Browse all recorded sessions with stats
- **Step list** -- Walk through each step with status icons, timing, and token counts
- **Context window viewer** -- See the exact context window at each step: every message, system prompt, and tool response the model saw
- **Visual diff** -- Timeline diff visualization with color-coded bars (Same / Modified / LeftOnly / RightOnly) and a side-by-side comparison table
- **WebSocket live** -- Watch recordings in real-time as your agent runs via WebSocket streaming, with auto-follow mode

## Everything Embedded in a Single Binary

The entire web UI is embedded in the single Rewind binary -- no Docker, no Node.js runtime needed. Just run `rewind web` and everything works out of the box.
