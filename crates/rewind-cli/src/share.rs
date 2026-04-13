//! Self-contained HTML generator for `rewind share`.
//!
//! Takes a serialized session and embeds it into a single HTML file with
//! an inline viewer. The result opens in any browser — no install, no
//! network, no dependencies.

use anyhow::Result;
use rewind_store::export::ExportedSession;

/// Generate a self-contained HTML file from an exported session.
pub fn generate_share_html(exported: &ExportedSession) -> Result<String> {
    let session_json = serde_json::to_string(exported)?;

    // Escape </script> to prevent closing the tag early.
    // No backslash escaping needed — <script type="application/json"> is raw text, not JS.
    let escaped_json = session_json.replace("</script>", "<\\/script>");

    Ok(format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Rewind — {session_name}</title>
{STYLE}
</head>
<body>
<div id="app"></div>
<script type="application/json" id="session-data">{escaped_json}</script>
{SCRIPT}
</body>
</html>"#,
        session_name = exported.session.name,
        STYLE = VIEWER_STYLE,
        escaped_json = escaped_json,
        SCRIPT = VIEWER_SCRIPT,
    ))
}

const VIEWER_STYLE: &str = r##"<style>
:root {
  --bg: #0d1117; --bg2: #161b22; --bg3: #21262d;
  --fg: #c9d1d9; --fg2: #8b949e; --fg3: #484f58;
  --cyan: #58a6ff; --green: #3fb950; --red: #f85149;
  --yellow: #d29922; --purple: #bc8cff; --blue: #58a6ff;
  --orange: #d18616;
}
* { box-sizing: border-box; margin: 0; padding: 0; }
body { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', monospace; background: var(--bg); color: var(--fg); line-height: 1.5; }
#app { max-width: 960px; margin: 0 auto; padding: 24px; }
.header { border-bottom: 1px solid var(--bg3); padding-bottom: 16px; margin-bottom: 20px; }
.header h1 { font-size: 18px; color: var(--cyan); }
.header h1 .emoji { margin-right: 6px; }
.meta { display: grid; grid-template-columns: repeat(auto-fill, minmax(180px, 1fr)); gap: 8px; margin: 12px 0; }
.meta-item { font-size: 13px; }
.meta-item .label { color: var(--fg2); }
.meta-item .value { color: var(--fg); font-weight: 600; }
.badge { display: inline-block; padding: 2px 8px; border-radius: 12px; font-size: 11px; font-weight: 600; }
.badge-green { background: rgba(63,185,80,.15); color: var(--green); }
.badge-red { background: rgba(248,81,73,.15); color: var(--red); }
.badge-yellow { background: rgba(210,153,34,.15); color: var(--yellow); }
.badge-blue { background: rgba(88,166,255,.15); color: var(--cyan); }
.badge-purple { background: rgba(188,140,255,.15); color: var(--purple); }
.content-notice { background: rgba(210,153,34,.1); border: 1px solid var(--yellow); border-radius: 6px; padding: 8px 12px; font-size: 12px; color: var(--yellow); margin-bottom: 16px; }
.section { margin-bottom: 24px; }
.section-title { font-size: 14px; color: var(--fg2); text-transform: uppercase; letter-spacing: 0.5px; margin-bottom: 8px; border-bottom: 1px solid var(--bg3); padding-bottom: 4px; }
.timeline { margin-bottom: 16px; }
.timeline-label { font-size: 13px; color: var(--purple); margin-bottom: 6px; }
.step { display: grid; grid-template-columns: 30px 20px 1fr 90px 70px 90px; align-items: center; padding: 6px 8px; border-radius: 4px; font-size: 13px; border-left: 2px solid var(--bg3); margin-left: 8px; }
.step:hover { background: var(--bg2); }
.step .num { color: var(--fg3); font-size: 12px; text-align: right; padding-right: 8px; }
.step .icon { text-align: center; }
.step .name { color: var(--fg); font-weight: 500; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.step .model { color: var(--purple); font-size: 12px; text-align: right; }
.step .dur { color: var(--yellow); font-size: 12px; text-align: right; }
.step .tokens { color: var(--blue); font-size: 12px; text-align: right; }
.step.error { border-left-color: var(--red); }
.step-error { font-size: 12px; color: var(--red); margin-left: 60px; padding: 2px 8px; }
.step-content { margin-left: 60px; padding: 8px; background: var(--bg2); border-radius: 4px; margin-top: 4px; margin-bottom: 4px; font-size: 12px; max-height: 300px; overflow: auto; }
.step-content pre { white-space: pre-wrap; word-break: break-word; color: var(--fg2); }
.step-content-toggle { margin-left: 60px; font-size: 11px; color: var(--fg3); cursor: pointer; user-select: none; }
.step-content-toggle:hover { color: var(--cyan); }
.span-tree { margin-left: 16px; border-left: 1px dashed var(--bg3); padding-left: 12px; }
.span-node { margin-bottom: 4px; }
.span-header { font-size: 13px; padding: 4px 6px; border-radius: 4px; cursor: default; }
.span-header:hover { background: var(--bg2); }
.span-name { font-weight: 600; color: var(--fg); }
.span-type { color: var(--fg3); font-size: 11px; }
.span-dur { color: var(--yellow); font-size: 12px; }
.scores { display: grid; grid-template-columns: repeat(auto-fill, minmax(200px, 1fr)); gap: 8px; }
.score-card { background: var(--bg2); border-radius: 6px; padding: 12px; }
.score-card .eval { font-size: 13px; color: var(--fg2); }
.score-card .value { font-size: 24px; font-weight: 700; }
.score-card .reason { font-size: 12px; color: var(--fg3); margin-top: 4px; }
.savings { background: var(--bg2); border-radius: 6px; padding: 12px; margin-bottom: 16px; }
.savings h3 { font-size: 13px; color: var(--cyan); margin-bottom: 6px; }
.savings .stat { display: inline-block; margin-right: 20px; font-size: 13px; }
.savings .stat .val { font-weight: 700; }
.footer { border-top: 1px solid var(--bg3); padding-top: 12px; margin-top: 24px; font-size: 11px; color: var(--fg3); text-align: center; }
.footer a { color: var(--cyan); text-decoration: none; }
</style>"##;

const VIEWER_SCRIPT: &str = r##"<script>
(function() {
  const data = JSON.parse(document.getElementById('session-data').textContent);
  const app = document.getElementById('app');
  if (data.export_version > 1) {
    const p = document.createElement('p');
    p.style.cssText = 'color:var(--yellow);padding:40px;text-align:center';
    p.textContent = 'This file was exported with a newer version of Rewind. Please update to view it.';
    app.appendChild(p);
    return;
  }
  const s = data.session;
  const hasContent = data.include_content;

  function esc(str) { const d = document.createElement('div'); d.textContent = str; return d.innerHTML; }
  function fmtDur(ms) { return ms >= 60000 ? `${Math.floor(ms/60000)}m ${Math.floor((ms%60000)/1000)}s` : ms >= 1000 ? `${(ms/1000).toFixed(1)}s` : `${ms}ms`; }
  function fmtTokens(n) { return n >= 1000 ? `${(n/1000).toFixed(1)}k` : `${n}`; }
  function stepIcon(t) { return {LlmCall:'🤖',ToolCall:'🔧',ToolResult:'📋',UserPrompt:'💬',HookEvent:'⚡'}[t]||'●'; }
  function statusBadge(st) { return st==='Completed'||st==='completed'?'badge-green':st==='Failed'||st==='failed'?'badge-red':'badge-yellow'; }

  let html = `<div class="header">
    <h1><span class="emoji">⏪</span>Rewind — ${esc(s.name)}</h1>
    <div class="meta">
      <div class="meta-item"><span class="label">ID:</span> <span class="value">${esc(s.id.slice(0,8))}</span></div>
      <div class="meta-item"><span class="label">Status:</span> <span class="badge ${statusBadge(s.status)}">${esc(typeof s.status==='string'?s.status:Object.keys(s.status)[0])}</span></div>
      <div class="meta-item"><span class="label">Steps:</span> <span class="value">${s.total_steps}</span></div>
      <div class="meta-item"><span class="label">Tokens:</span> <span class="value">${fmtTokens(s.total_tokens)}</span></div>
      <div class="meta-item"><span class="label">Created:</span> <span class="value">${new Date(s.created_at).toLocaleString()}</span></div>
    </div>`;
  if (hasContent) html += `<div class="content-notice">⚠ This file contains full LLM request/response content.</div>`;
  html += `</div>`;

  // Timelines + steps
  html += `<div class="section"><div class="section-title">Timelines</div>`;
  for (const tl of data.timelines) {
    const t = tl.timeline;
    const isFork = !!t.parent_timeline_id;
    html += `<div class="timeline">`;
    html += `<div class="timeline-label">${isFork?'⑂ Fork':'◉ Root'}: ${esc(t.label)} ${isFork?`(from step ${t.fork_at_step})`:''}</div>`;
    for (const es of tl.steps) {
      const st = es.step || es;
      const isErr = (typeof st.status==='string'?st.status:Object.keys(st.status)[0])==='Error'||(typeof st.status==='string'?st.status:Object.keys(st.status)[0])==='error';
      const typ = typeof st.step_type==='string'?st.step_type:Object.keys(st.step_type)[0];
      html += `<div class="step${isErr?' error':''}">
        <span class="num">${st.step_number}</span>
        <span class="icon">${stepIcon(typ)}</span>
        <span class="name">${esc(typ)}${st.tool_name?` — ${esc(st.tool_name)}`:''}</span>
        <span class="model">${esc(st.model||'')}</span>
        <span class="dur">${fmtDur(st.duration_ms)}</span>
        <span class="tokens">${fmtTokens(st.tokens_in)}↓ ${fmtTokens(st.tokens_out)}↑</span>
      </div>`;
      if (isErr && st.error) html += `<div class="step-error">✗ ${esc(st.error)}</div>`;
      if (hasContent && (es.request_content || es.response_content)) {
        const sid = `step-${t.id}-${st.step_number}`;
        html += `<div class="step-content-toggle" onclick="document.getElementById('${sid}').style.display=document.getElementById('${sid}').style.display==='none'?'block':'none'">▸ Show content</div>`;
        html += `<div class="step-content" id="${sid}" style="display:none">`;
        if (es.request_content) html += `<div><strong>Request:</strong><pre>${esc(JSON.stringify(es.request_content,null,2))}</pre></div>`;
        if (es.response_content) html += `<div style="margin-top:8px"><strong>Response:</strong><pre>${esc(JSON.stringify(es.response_content,null,2))}</pre></div>`;
        html += `</div>`;
      }
    }
    html += `</div>`;
  }
  html += `</div>`;

  // Spans
  if (data.spans && data.spans.length > 0) {
    html += `<div class="section"><div class="section-title">Span Tree</div>`;
    const roots = data.spans.filter(s => !s.parent_span_id);
    function renderSpan(span) {
      const children = data.spans.filter(s => s.parent_span_id === span.id);
      const typ = typeof span.span_type==='string'?span.span_type:Object.keys(span.span_type)[0];
      let h = `<div class="span-node"><div class="span-header">
        ${children.length?'▾':'•'} <span class="span-name">${esc(span.name)}</span>
        <span class="span-type">${esc(typ)}</span>
        <span class="span-dur">${fmtDur(span.duration_ms)}</span>
        ${span.error?`<span style="color:var(--red)">✗ ${esc(span.error)}</span>`:''}
      </div>`;
      if (children.length) { h += `<div class="span-tree">`; for (const c of children) h += renderSpan(c); h += `</div>`; }
      h += `</div>`;
      return h;
    }
    for (const r of roots) html += renderSpan(r);
    html += `</div>`;
  }

  // Scores
  if (data.scores && data.scores.length > 0) {
    html += `<div class="section"><div class="section-title">Evaluation Scores</div><div class="scores">`;
    for (const sc of data.scores) {
      html += `<div class="score-card">
        <div class="eval">${esc(sc.evaluator_id)}</div>
        <div class="value" style="color:${sc.passed?'var(--green)':'var(--red)'}">${sc.score.toFixed(2)} ${sc.passed?'✓':'✗'}</div>
        ${sc.reasoning?`<div class="reason">${esc(sc.reasoning.slice(0,200))}</div>`:''}
      </div>`;
    }
    html += `</div></div>`;
  }

  // Footer
  html += `<div class="footer">Generated by <a href="https://github.com/agentoptics/rewind">⏪ Rewind</a> — time-travel debugger for AI agents</div>`;

  app.innerHTML = html;
})();
</script>"##;

#[cfg(test)]
mod tests {
    use super::*;
    use rewind_store::export::ExportedSession;
    use rewind_store::*;
    use chrono::Utc;

    fn make_test_export(include_content: bool) -> ExportedSession {
        let session = Session {
            id: "sess-test-1234".into(),
            name: "test-agent-debug".into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            status: SessionStatus::Completed,
            source: SessionSource::Direct,
            total_steps: 2,
            total_tokens: 300,
            metadata: serde_json::json!({}),
            thread_id: None,
            thread_ordinal: None,
        };

        let tl = Timeline::new_root(&session.id);
        let step1 = rewind_store::export::ExportedStep {
            step: Step {
                id: "step-1".into(),
                timeline_id: tl.id.clone(),
                session_id: session.id.clone(),
                step_number: 1,
                step_type: StepType::LlmCall,
                status: StepStatus::Success,
                created_at: Utc::now(),
                duration_ms: 500,
                tokens_in: 100,
                tokens_out: 50,
                model: "gpt-4o".into(),
                request_blob: String::new(),
                response_blob: String::new(),
                error: None,
                span_id: None,
                tool_name: None,
            },
            request_content: if include_content {
                Some(serde_json::json!({"messages": [{"role": "user", "content": "hello"}]}))
            } else {
                None
            },
            response_content: if include_content {
                Some(serde_json::json!({"choices": [{"message": {"content": "hi"}}]}))
            } else {
                None
            },
        };

        ExportedSession {
            session,
            timelines: vec![rewind_store::export::ExportedTimeline {
                timeline: tl,
                steps: vec![step1],
            }],
            spans: vec![],
            scores: vec![],
            include_content,
            export_version: 1,
        }
    }

    #[test]
    fn generates_valid_html() {
        let exported = make_test_export(false);
        let html = generate_share_html(&exported).unwrap();

        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.contains("<title>Rewind — test-agent-debug</title>"));
        assert!(html.contains("session-data"));
        assert!(html.contains("gpt-4o"));
    }

    #[test]
    fn metadata_only_no_content_warning() {
        let exported = make_test_export(false);
        let html = generate_share_html(&exported).unwrap();

        // include_content is false in the JSON
        assert!(html.contains(r#""include_content":false"#));
    }

    #[test]
    fn with_content_includes_blobs() {
        let exported = make_test_export(true);
        let html = generate_share_html(&exported).unwrap();

        assert!(html.contains("hello")); // from request content
        assert!(html.contains(r#""include_content":true"#));
    }

    #[test]
    fn escapes_script_tags() {
        // Ensure </script> in content doesn't break the HTML
        let mut exported = make_test_export(true);
        exported.timelines[0].steps[0].response_content =
            Some(serde_json::json!({"text": "</script><script>alert(1)</script>"}));

        let html = generate_share_html(&exported).unwrap();
        // Should NOT contain literal </script> inside the JSON block
        let json_start = html.find(r#"<script type="application/json""#).unwrap();
        let json_end = html[json_start..].find("</script>").unwrap();
        let json_block = &html[json_start..json_start + json_end];
        assert!(!json_block.contains("</script><script>"));
    }

    #[test]
    fn html_size_reasonable() {
        let exported = make_test_export(false);
        let html = generate_share_html(&exported).unwrap();
        // Base template + small session should be well under 100KB
        assert!(html.len() < 100_000, "HTML too large: {} bytes", html.len());
    }
}
