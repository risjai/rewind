import { useState, useEffect } from 'react'
import { api } from '@/lib/api'
import { cn } from '@/lib/utils'
import { X, Upload, CheckCircle2, AlertCircle, Loader2 } from 'lucide-react'
import type { Timeline } from '@/types/api'

interface Props {
  isOpen: boolean
  onClose: () => void
  sessionId: string
  timelines: Timeline[]
}

type ExportState = 'idle' | 'exporting' | 'success' | 'error'

export function ExportOtelModal({ isOpen, onClose, sessionId, timelines }: Props) {
  const [endpoint, setEndpoint] = useState('')
  const [includeContent, setIncludeContent] = useState(false)
  const [timelineMode, setTimelineMode] = useState<'main' | 'all' | 'specific'>('main')
  const [selectedTimeline, setSelectedTimeline] = useState<string>('')
  const [state, setState] = useState<ExportState>('idle')
  const [result, setResult] = useState<{ spans_exported: number; trace_id: string } | null>(null)
  const [error, setError] = useState<string>('')

  // Escape key to close
  useEffect(() => {
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') handleClose()
    }
    if (isOpen) document.addEventListener('keydown', handleKey)
    return () => document.removeEventListener('keydown', handleKey)
  }, [isOpen])

  if (!isOpen) return null

  const handleExport = async () => {
    setState('exporting')
    setError('')
    setResult(null)

    try {
      const opts: { endpoint?: string; include_content?: boolean; timeline_id?: string | null; all_timelines?: boolean } = {
        include_content: includeContent,
      }
      if (endpoint.trim()) {
        opts.endpoint = endpoint.trim()
      }
      if (timelineMode === 'all') {
        opts.all_timelines = true
      } else if (timelineMode === 'specific' && selectedTimeline) {
        opts.timeline_id = selectedTimeline
      }

      const res = await api.exportOtel(sessionId, opts)
      setResult(res)
      setState('success')
    } catch (e) {
      const msg = e instanceof Error ? e.message : 'Export failed'
      if (msg.startsWith('API error 501')) {
        setError('No endpoint provided. Enter a collector URL above or set REWIND_OTEL_ENDPOINT on the server.')
      } else {
        setError(msg)
      }
      setState('error')
    }
  }

  const handleClose = () => {
    setState('idle')
    setResult(null)
    setError('')
    setEndpoint('')
    onClose()
  }

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      {/* Backdrop */}
      <div className="absolute inset-0 bg-black/60" onClick={handleClose} />

      {/* Modal */}
      <div role="dialog" aria-modal="true" aria-label="Export to OpenTelemetry" className="relative bg-neutral-900 border border-neutral-700 rounded-xl shadow-2xl w-full max-w-md mx-4">
        {/* Header */}
        <div className="flex items-center justify-between px-5 py-4 border-b border-neutral-800">
          <div className="flex items-center gap-2">
            <Upload size={16} className="text-cyan-400" />
            <h3 className="text-sm font-semibold text-neutral-200">Export to OpenTelemetry</h3>
          </div>
          <button onClick={handleClose} className="text-neutral-500 hover:text-neutral-300 transition-colors">
            <X size={16} />
          </button>
        </div>

        {/* Body */}
        <div className="px-5 py-4 space-y-4">
          {/* Endpoint URL */}
          <div>
            <label className="block text-xs font-medium text-neutral-400 mb-1.5">Collector endpoint</label>
            <input
              type="url"
              value={endpoint}
              onChange={(e) => setEndpoint(e.target.value)}
              placeholder="http://localhost:4318 (or set REWIND_OTEL_ENDPOINT)"
              className="w-full bg-neutral-800 border border-neutral-700 rounded-lg px-3 py-1.5 text-xs text-neutral-200 placeholder:text-neutral-500 focus:border-cyan-600 focus:outline-none focus:ring-1 focus:ring-cyan-600"
            />
          </div>

          {/* Timeline selection */}
          {timelines.length > 1 && (
            <div>
              <label className="block text-xs font-medium text-neutral-400 mb-2">Timeline</label>
              <div className="flex gap-2">
                {(['main', 'all', 'specific'] as const).map((mode) => (
                  <button
                    key={mode}
                    onClick={() => setTimelineMode(mode)}
                    className={cn(
                      'px-3 py-1.5 rounded-lg text-xs transition-colors',
                      timelineMode === mode
                        ? 'bg-neutral-700 text-neutral-100'
                        : 'text-neutral-400 hover:bg-neutral-800 hover:text-neutral-200'
                    )}
                  >
                    {mode === 'main' ? 'Main' : mode === 'all' ? 'All timelines' : 'Specific'}
                  </button>
                ))}
              </div>
              {timelineMode === 'specific' && (
                <select
                  value={selectedTimeline}
                  onChange={(e) => setSelectedTimeline(e.target.value)}
                  className="mt-2 w-full bg-neutral-800 border border-neutral-700 rounded-lg px-3 py-1.5 text-xs text-neutral-200"
                >
                  <option value="">Select timeline...</option>
                  {timelines.map((tl) => (
                    <option key={tl.id} value={tl.id}>{tl.label} ({tl.id.slice(0, 8)})</option>
                  ))}
                </select>
              )}
            </div>
          )}

          {/* Include content toggle */}
          <label className="flex items-center gap-2 cursor-pointer">
            <input
              type="checkbox"
              checked={includeContent}
              onChange={(e) => setIncludeContent(e.target.checked)}
              className="rounded border-neutral-600 bg-neutral-800 text-cyan-500 focus:ring-cyan-500"
            />
            <span className="text-xs text-neutral-300">Include message content</span>
            <span className="text-[10px] text-amber-500">(sensitive)</span>
          </label>

          {/* Success message */}
          {state === 'success' && result && (
            <div className="flex items-start gap-2 bg-green-950/30 border border-green-900/50 rounded-lg px-3 py-2.5">
              <CheckCircle2 size={14} className="text-green-400 mt-0.5 shrink-0" />
              <div className="text-xs">
                <p className="text-green-300 font-medium">Exported {result.spans_exported} spans</p>
                <p className="text-green-400/70 mt-0.5 font-mono">{result.trace_id.slice(0, 32)}</p>
              </div>
            </div>
          )}

          {/* Error message */}
          {state === 'error' && error && (
            <div className="flex items-start gap-2 bg-red-950/30 border border-red-900/50 rounded-lg px-3 py-2.5">
              <AlertCircle size={14} className="text-red-400 mt-0.5 shrink-0" />
              <p className="text-xs text-red-300">{error}</p>
            </div>
          )}
        </div>

        {/* Footer */}
        <div className="flex justify-end gap-2 px-5 py-3 border-t border-neutral-800">
          <button
            onClick={handleClose}
            className="px-3 py-1.5 rounded-lg text-xs text-neutral-400 hover:bg-neutral-800 hover:text-neutral-200 transition-colors"
          >
            {state === 'success' ? 'Done' : 'Cancel'}
          </button>
          {state !== 'success' && (
            <button
              onClick={handleExport}
              disabled={state === 'exporting'}
              className={cn(
                'flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-xs font-medium transition-colors',
                state === 'exporting'
                  ? 'bg-neutral-700 text-neutral-400 cursor-not-allowed'
                  : 'bg-cyan-600 text-white hover:bg-cyan-500'
              )}
            >
              {state === 'exporting' ? (
                <><Loader2 size={12} className="animate-spin" /> Exporting...</>
              ) : (
                <><Upload size={12} /> Export</>
              )}
            </button>
          )}
        </div>
      </div>
    </div>
  )
}
