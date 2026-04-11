# Slack Research: Agent Observability Tools & Discussions

**Date:** 2026-04-11
**Researcher:** Rishabh Jain
**Method:** Targeted Slack search across internal channels using keywords: "agent observability", "agent tracing", "Claude Code", "agent debugging", "LLM observability", "agents-observe", and related terms. Channels were ranked by signal relevance to Rewind's positioning as a time-travel debugger for AI agents.

---

## Executive Summary

There is significant internal demand for agent observability tooling. Engineers are struggling with opaque agent behavior, debugging long-running sessions, and tracking costs. Multiple teams have built or adopted observability solutions independently, but no single tool addresses the full workflow of record-replay-debug that Rewind offers. The strongest overlap with Rewind's feature set comes from Lu Han's explicit wishlist and Jacob Park's debugging pain points.

---

## Channels by Signal Strength

### High Signal

| Channel | ID | Why It Matters |
|---|---|---|
| #community-claude-code | C09BGJDQPAR | Largest concentration of agent power users. Active cost tracking and observability discussions. |
| #agentforce-ai-tools-moments | C0AGKN9NMDL | Tool-sharing channel with receptive audience. agents-observe was posted and discussed here. |
| #data-foundation-solutions-global-team | C02H67KE5 | Comprehensive observability tool comparison posted by Rika Ng. Customer demand signal from Aizaz Alam. |
| #observability--proj_sidecar--3p_agents_for_observability_and_adlc--claude_code | C0ANXCB8TQA | Dedicated channel for 3P agent observability + Claude Code. Lu Han building AF Observability skill for TDX. |
| #tmp-sdb-general / #sdb-ai-agent-test | -- | Yong Liu's team built "Touchstone" -- production-grade AI agent evaluation system. |

### Medium Signal

| Channel | ID | Why It Matters |
|---|---|---|
| #agentforce-full-session-tracing | C086TEJTSCF | Agentforce Session Trace OTel API launched in Beta (April 2026). |
| #internal-agents-collaboration | C081CF78Y4F | Jacob Park reporting Engineering Agent hangs 10-20% of the time with zero visibility. |
| #sl-ideation | C0915AU53V1 | Boris Litvak mentions Arize Phoenix used by Python Planner team. |
| #cecm-eng-standup-water-cooler | C04DN1HQAA2 | AI Cost Tracker shared for managing Cursor/Claude usage. |
| #agentforce-observability | C08JQNVLXD1 | Multiple Agentforce observability channels exist; broader organizational investment. |

### Lower Signal

| Channel | ID | Why It Matters |
|---|---|---|
| #proj-adk | C06HAB05X0S | Mostly Salesforce ADK tooling issues, not general agent observability. |

---

## Key People

| Person | Slack ID | Role / Relevance |
|---|---|---|
| **Carson Kahn** | U0A1E4PPLAC | Built [agents-observe](https://github.com/simple10/agents-observe) for real-time multi-agent observability. Active in the observability space. |
| **Lu Han** | U92LAG59U | Tested agents-observe. Building AF Observability skills. Gave a detailed feature wishlist that directly maps to Rewind capabilities. |
| **Yong Liu** | U0699T1DAKA | Built "Touchstone" -- production AI agent evaluation system with auto-tracing, 130+ scorers, MLflow integration, weekly automated reporting across 10+ channels. |
| **Ryan Atallah** | U92S9EBBR | Leads Agentforce Observability team. |
| **Rika Ng** | WJ96MUQHG | Posted comprehensive comparison of 8 LLM observability tools. |
| **Aizaz Alam** | U02A5GQB2S2 | Noted customer demand: "customers are really concerned about being able to track all AI LLM conversations & messages." |
| **Jacob Park** | W018DE50CBW | Frustrated with Engineering Agent debugging -- 10-20% hang rate with zero visibility. Real pain-point user. |

---

## Pain Points Mapped to Rewind

| # | Pain Point | Source | Rewind Feature |
|---|---|---|---|
| 1 | "How are our agents actually performing?" -- no systematic answer | Touchstone motivation | Session recording + evaluation framework |
| 2 | No visibility into long-running agents | Lu Han's explicit ask | Real-time session replay, step-by-step trace |
| 3 | Agent hangs with no explanation (10-20% failure rate) | Jacob Park | Time-travel debugging, snapshot inspection |
| 4 | Can't inspect agent I/O at each step | Lu Han's wishlist | Full request/response capture per step |
| 5 | Expensive re-runs for debugging | Claude Code budget discussions | Replay and forking from any snapshot |
| 6 | No session reconstruction for human review | Rika Ng's comparison context | Web UI session viewer |
| 7 | Regression detection across agent updates | Touchstone built weekly automated evals | Assertion baselines + regression testing |
| 8 | Cost tracking per agent run | Multiple threads | Token/cost metadata in recordings |

### Lu Han's Wishlist (verbatim excerpts)

These quotes are particularly relevant because they describe Rewind's existing feature set almost exactly:

- "manage claude code sessions"
- "understand what the long running agent is performing"
- "inspect the trace with concrete input/output"
- "group multiple selected tools usage traces"

---

## Notable Projects

### agents-observe (Carson Kahn)

- **Repo:** https://github.com/simple10/agents-observe
- **What:** Real-time multi-agent observability tool
- **Status:** Actively shared in #community-claude-code and #agentforce-ai-tools-moments
- **Gap vs Rewind:** Lacks time-travel (replay/fork), regression testing, and evaluation framework

### Touchstone (Yong Liu's team)

- **What:** Production-grade AI agent evaluation system
- **Features:** Auto-tracing, 130+ scorers, MLflow integration, weekly automated reporting across 10+ channels
- **Gap vs Rewind:** Evaluation-focused; no interactive session debugging or replay

### Agentforce Session Trace OTel API

- **Status:** Beta launch (April 2026)
- **What:** OpenTelemetry-based tracing for Agentforce sessions
- **Relevance:** Sets a standard for trace export; Rewind could consume these traces

---

## Tool Landscape Comparison

From Rika Ng's comprehensive post in #data-foundation-solutions-global-team:

| Tool | Positioning | Open Source | Key Strength |
|---|---|---|---|
| **LangSmith** | Gold standard for agentic workflows | No | Native LangChain integration, visualization |
| **Langfuse** | Open-source observability | Yes | Session tracking, cost monitoring, self-hostable |
| **Braintrust** | Enterprise tracing | No | Used by Stripe/Zapier |
| **Helicone** | Lightweight proxy | No | Instant setup, minimal code changes |
| **Portkey** | Reliability platform | No | Automatic fallbacks, gateway |
| **Arize Phoenix** | ML observability | Yes | Drift detection, RAG metrics |
| **Galileo AI** | Quality assurance | No | Hallucination detection |
| **DeepEval (Confident AI)** | Evaluation-first | Yes | Turns production logs into test datasets |

### Where Rewind Differentiates

None of the above tools offer:
- **Time-travel debugging** -- replay from any point, fork to explore alternatives
- **Deterministic replay** -- reproduce exact agent behavior without re-calling LLMs
- **Framework-agnostic recording** -- works across OpenAI Agents SDK, LangChain, custom setups
- **Local-first architecture** -- no data leaves the developer's machine unless explicitly shared
- **Regression testing from recordings** -- assert against known-good sessions

---

## Recommended Channels for Rewind Announcements

Ranked by audience fit and receptiveness:

| Priority | Channel | Rationale |
|---|---|---|
| 1 | #community-claude-code | Most directly relevant -- engineers actively using Claude Code and seeking observability |
| 2 | #agentforce-ai-tools-moments | Tool-sharing culture, receptive audience, Carson Kahn + Lu Han active here |
| 3 | #ai-club | Broader AI discussion, good for awareness |
| 4 | #data-foundation-solutions-global-team | People actively comparing observability tools, open to new options |

---

## Next Steps

- [ ] Reach out to Lu Han -- her wishlist is almost a product spec for Rewind
- [ ] Connect with Carson Kahn -- potential collaborator or early adopter
- [ ] Share Rewind demo in #community-claude-code with positioning against agents-observe gaps
- [ ] Explore OTel trace import to consume Agentforce Session Trace data
- [ ] Follow up with Jacob Park on Engineering Agent debugging pain points
