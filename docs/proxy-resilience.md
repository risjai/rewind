# Proxy Resilience

When using proxy mode (`rewind record`), all LLM calls route through the Rewind proxy. This page explains what happens when the proxy becomes unavailable, and how to deploy for maximum reliability.

---

## How proxy mode works

```
Your Agent  --->  Rewind Proxy (127.0.0.1:8443)  --->  LLM Provider
                        |
                  Records request/response
                  to local SQLite store
```

1. `rewind record` starts a transparent HTTP proxy on port 8443
2. Your agent's `OPENAI_BASE_URL` / `ANTHROPIC_BASE_URL` is pointed at the proxy
3. Every LLM call passes through the proxy, which forwards it to the upstream provider
4. The proxy records the full request, response, token counts, and timing
5. Streaming works in real-time with zero added latency (SSE pass-through)

---

## What happens when the proxy dies

Rewind's SDK has two layers of protection to ensure **your agent never stops working**, even if the proxy crashes:

### Init-time fallthrough

When `init(mode="proxy")` is called, the SDK pings the proxy's health endpoint (`/_rewind/health`) before redirecting traffic. If the proxy is unreachable:

- The SDK logs a warning: *"Rewind proxy not reachable. Falling back to direct recording mode."*
- It silently switches to direct mode (in-process monkey-patching)
- Your agent starts normally with direct recording active

### Mid-session circuit breaker

For long-running agents, the proxy could die after initialization. The SDK includes a circuit breaker that detects proxy failure mid-session:

| State | Behavior |
|:---|:---|
| **CLOSED** (normal) | All traffic routes through the proxy. Zero overhead. |
| **OPEN** (proxy down) | After 2 consecutive connection errors, the circuit trips. All calls bypass the proxy and go directly to the LLM provider via a throwaway client. Recording continues in direct mode to a local fallback session. |
| **HALF_OPEN** (probing) | After 30 seconds, the circuit breaker sends one call through the proxy. If it succeeds, the circuit closes and proxy mode resumes. If it fails, the circuit stays open. |

The circuit breaker detects failures from the SDK's own connection errors (`APIConnectionError`) -- no per-request health checks, no added latency on the happy path.

**What gets recorded during fallback:**

- Steps before the proxy died: recorded in the proxy's store
- Steps after the circuit trips: recorded in the local store (`~/.rewind/`) as a new session named `"<original> (proxy-fallback)"`

---

## Deployment patterns

### Dev workstation (default)

```bash
# Terminal 1
rewind record --name "my-agent" --upstream https://api.openai.com

# Terminal 2
export OPENAI_BASE_URL=http://127.0.0.1:8443/v1
python my_agent.py
```

If the proxy dies, restart it. The circuit breaker keeps the agent alive in the meantime.

### Docker sidecar

```yaml
services:
  rewind:
    image: ghcr.io/agentoptics/rewind:latest
    command: rewind record --port 8443 --upstream https://api.openai.com
    healthcheck:
      test: ["CMD", "curl", "-sf", "http://localhost:8443/_rewind/health"]
      interval: 10s
      retries: 3
    restart: unless-stopped

  agent:
    build: .
    environment:
      - OPENAI_BASE_URL=http://rewind:8443/v1
    depends_on:
      rewind:
        condition: service_healthy
```

If the sidecar dies, the SDK falls through to direct mode. Docker's `restart: unless-stopped` auto-recovers the proxy.

### Systemd service

```ini
[Unit]
Description=Rewind Recording Proxy
After=network.target

[Service]
ExecStart=/usr/local/bin/rewind record --port 8443 --upstream https://api.openai.com
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
```

With `Restart=always`, systemd automatically restarts the proxy on crash. The circuit breaker keeps the agent alive during the ~5s restart window.

---

## When to use direct mode instead

For **production deployments** where observability must not risk availability, use direct mode:

```python
import rewind_agent
rewind_agent.init(mode="direct")
```

| | Proxy mode | Direct mode |
|:---|:---|:---|
| **Network hop** | Yes (through proxy) | No |
| **Single point of failure** | Proxy process (mitigated by circuit breaker) | None |
| **Language support** | Any (HTTP proxy) | Python only |
| **Recording quality** | Full SSE capture, language-agnostic | SDK-level monkey-patches |
| **Best for** | Dev/staging, multi-language agents | Production, Python-only agents |

**Recommendation:** Use proxy mode in dev and staging where the richer recording features (streaming SSE capture, language-agnostic support) justify the hop. Use direct mode in production where availability is paramount.

---

## Health check endpoint

Both the proxy and web server expose a health endpoint:

```bash
curl http://127.0.0.1:8443/_rewind/health
# {"status": "ok", "version": "0.7.0", "session": "abc123", "steps": 42}
```

Use this for Docker healthchecks, load balancer probes, or monitoring.

---

## Configuration

| Parameter | Default | Description |
|:---|:---|:---|
| Circuit breaker threshold | 2 consecutive failures | Number of connection errors before tripping to OPEN |
| Recovery timeout | 30 seconds | Time before probing the proxy in HALF_OPEN state |
| Health check timeout | 0.5 seconds | Init-time health check timeout |

These defaults are suitable for most deployments. They are not currently user-configurable but may be exposed in a future release.
