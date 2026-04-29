import { useQuery } from '@tanstack/react-query'
import { api } from '@/lib/api'
import { cn, formatDuration, formatTokens } from '@/lib/utils'
import { useState, useEffect, useCallback } from 'react'
import { MessageSquare, FileJson, FileOutput, AlertTriangle, GitBranch, Play, Rocket, Pencil } from 'lucide-react'
import { JsonTree } from './JsonTree'
import { ForkModal } from './ForkModal'
import { ReplaySetupModal } from './ReplaySetupModal'
import { ReplayJobModal } from './RunReplayButton'
import { useStepEdit } from '@/hooks/use-step-edit'
import { useStore } from '@/hooks/use-store'

type Tab = 'context' | 'request' | 'response'
type ModalMode = 'fork' | 'replay' | 'runReplay' | null

export function StepDetailPanel({ stepId }: { stepId: string }) {
  const [tab, setTab] = useState<Tab | null>(null)
  const [modalMode, setModalMode] = useState<ModalMode>(null)
  const [editing, setEditing] = useState<'request' | 'response' | null>(null)
  const [editorText, setEditorText] = useState('')
  const [parseError, setParseError] = useState<string | null>(null)
  const [toastMsg, setToastMsg] = useState<string | null>(null)
  const [errorToastMsg, setErrorToastMsg] = useState<string | null>(null)
  const [originalText, setOriginalText] = useState('')
  const selectTimeline = useStore((s) => s.selectTimeline)
  const selectedTimelineId = useStore((s) => s.selectedTimelineId)

  const { data: step, isLoading } = useQuery({
    queryKey: ['step-detail', stepId],
    queryFn: () => api.stepDetail(stepId),
  })

  const { data: sessionForRoot } = useQuery({
    queryKey: ['session', step?.session_id],
    queryFn: () => api.session(step!.session_id),
    enabled: !!step?.session_id,
  })
  const rootTimelineId = sessionForRoot?.timelines.find(t => t.parent_timeline_id === null)?.id
  const contextTimelineId = selectedTimelineId ?? rootTimelineId ?? step?.timeline_id ?? ''

  const stepEdit = useStepEdit({
    stepId,
    sessionId: step?.session_id ?? '',
    timelineId: step?.timeline_id ?? '',
    contextTimelineId,
  })

  const startEditing = useCallback((field: 'request' | 'response', data: unknown) => {
    const text = JSON.stringify(data, null, 2)
    setEditing(field)
    setEditorText(text)
    setOriginalText(text)
    setParseError(null)
  }, [])

  const cancelEditing = useCallback(() => {
    setEditing(null)
    setEditorText('')
    setOriginalText('')
    setParseError(null)
  }, [])

  const handleEditorChange = useCallback((value: string) => {
    setEditorText(value)
    try {
      JSON.parse(value)
      setParseError(null)
    } catch (e) {
      setParseError(e instanceof Error ? e.message : 'Invalid JSON')
    }
  }, [])

  const textChanged = editorText !== originalText
  const canSave = !parseError && textChanged

  const handleSaveClick = useCallback(() => {
    if (!editing || !canSave) return
    stepEdit.openConfirm(editing, editorText)
  }, [editing, canSave, editorText, stepEdit])

  const handleConfirm = useCallback(async () => {
    const result = await stepEdit.save()
    if (result) {
      if (result.deleted_downstream_count > 0 && step) {
        setToastMsg(
          `Cleared ${result.deleted_downstream_count} step(s) after #${step.step_number}. Run replay to populate them.`,
        )
      }
      if (stepEdit.autoForked && stepEdit.forkTimelineId) {
        selectTimeline(stepEdit.forkTimelineId)
      }
      cancelEditing()
    }
  }, [stepEdit, step, cancelEditing, selectTimeline])

  if (isLoading) {
    return <div className="flex items-center justify-center h-full text-neutral-500 text-sm">Loading step...</div>
  }
  if (!step) {
    return <div className="flex items-center justify-center h-full text-neutral-500 text-sm">Step not found</div>
  }

  // Default tab: 'context' for LLM calls (have messages), 'request' for hook steps
  const isHookStep = step.step_type === 'user_prompt' || step.step_type === 'hook_event' || (!step.messages && step.request_body)
  const activeTab = tab ?? (isHookStep ? 'request' : 'context')

  // Derive confirm modal copy
  const confirmProps = (() => {
    if (stepEdit.onMain && !stepEdit.allowMainEdits) {
      return {
        title: 'Create fork and edit',
        body: 'Main timelines are protected. Saving will create a fork and switch you there.',
        confirmLabel: 'Fork & save',
        variant: 'default' as const,
      }
    }
    if (stepEdit.onMain && stepEdit.allowMainEdits) {
      return {
        title: 'Edit main timeline (destructive)',
        body: `This will mutate the original recording AND delete ${stepEdit.cascadeCount} downstream step(s). This cannot be undone.`,
        confirmLabel: 'Save destructively',
        variant: 'destructive' as const,
      }
    }
    return {
      title: 'Save edit',
      body: `This will save the new JSON and clear ${stepEdit.cascadeCount} downstream step(s). Run replay afterwards.`,
      confirmLabel: 'Save',
      variant: 'default' as const,
    }
  })()

  return (
    <div className="flex flex-col h-full">
      {/* Step info header */}
      <div className="px-4 py-3 border-b border-neutral-800 space-y-2">
        <div className="flex items-center gap-3">
          <span className="text-sm font-mono text-neutral-500">Step #{step.step_number}</span>
          <span className="text-sm font-medium text-neutral-200">{step.step_type}</span>
          {step.tool_name && (
            <span className="text-xs bg-violet-950/50 text-violet-300 px-1.5 py-0.5 rounded border border-violet-800/50 font-mono">{step.tool_name}</span>
          )}
          {step.model && <span className="text-xs bg-neutral-800 text-neutral-400 px-1.5 py-0.5 rounded font-mono">{step.model}</span>}
          <StatusPill status={step.status} />
          <div className="ml-auto flex items-center gap-1.5">
            <button
              onClick={() => setModalMode('fork')}
              title={`Fork a new timeline inheriting steps 1–${step.step_number}`}
              className="flex items-center gap-1 text-[11px] text-amber-400 hover:text-amber-300 border border-amber-900/50 hover:border-amber-700 bg-amber-950/20 hover:bg-amber-950/40 px-2 py-0.5 rounded-md transition-colors"
            >
              <GitBranch size={11} /> Fork from here
            </button>
            <button
              onClick={() => setModalMode('replay')}
              title={`Set up a replay from step ${step.step_number}: cached replay for steps 1–${step.step_number}, live upstream after`}
              className="flex items-center gap-1 text-[11px] text-cyan-400 hover:text-cyan-300 border border-cyan-900/50 hover:border-cyan-700 bg-cyan-950/20 hover:bg-cyan-950/40 px-2 py-0.5 rounded-md transition-colors"
            >
              <Play size={11} /> Set up replay…
            </button>
            <button
              onClick={() => setModalMode('runReplay')}
              title={`Dispatch this replay to a registered runner (e.g. ray-agent). Re-executes the agent against the recorded LLM cache; new steps land on a fresh fork timeline.`}
              className="flex items-center gap-1 text-[11px] text-emerald-400 hover:text-emerald-300 border border-emerald-900/50 hover:border-emerald-700 bg-emerald-950/20 hover:bg-emerald-950/40 px-2 py-0.5 rounded-md transition-colors"
            >
              <Rocket size={11} /> Run replay
            </button>
          </div>
        </div>
        <div className="flex items-center gap-4 text-xs text-neutral-500">
          <span>{formatDuration(step.duration_ms)}</span>
          {(step.tokens_in > 0 || step.tokens_out > 0) && (
            <span>{formatTokens(step.tokens_in)} in / {formatTokens(step.tokens_out)} out</span>
          )}
        </div>
        {step.error && (
          <div className="flex items-center gap-1.5 text-xs text-red-400 bg-red-950/30 px-2.5 py-1.5 rounded border border-red-900/50">
            <AlertTriangle size={12} />
            {step.error}
          </div>
        )}
      </div>

      {/* Tabs */}
      <div className="flex border-b border-neutral-800">
        <TabButton icon={MessageSquare} label="Context Window" active={activeTab === 'context'} onClick={() => setTab('context')} />
        <TabButton icon={FileJson} label="Request" active={activeTab === 'request'} onClick={() => setTab('request')}
          editButton={step.request_body != null && editing !== 'request'
            ? () => startEditing('request', step.request_body)
            : undefined}
        />
        <TabButton icon={FileOutput} label="Response" active={activeTab === 'response'} onClick={() => setTab('response')}
          editButton={step.response_body != null && editing !== 'response'
            ? () => startEditing('response', step.response_body)
            : undefined}
        />
      </div>

      {/* Tab content */}
      <div className="flex-1 overflow-auto scrollbar-thin">
        {activeTab === 'context' && <ContextWindowView messages={step.messages} />}
        {activeTab === 'request' && (
          editing === 'request' ? (
            <JsonEditor
              text={editorText}
              onChange={handleEditorChange}
              parseError={parseError}
              canSave={canSave}
              onSave={handleSaveClick}
              onCancel={cancelEditing}
            />
          ) : (
            <JsonView data={step.request_body} label="Request" />
          )
        )}
        {activeTab === 'response' && (
          editing === 'response' ? (
            <JsonEditor
              text={editorText}
              onChange={handleEditorChange}
              parseError={parseError}
              canSave={canSave}
              onSave={handleSaveClick}
              onCancel={cancelEditing}
            />
          ) : (
            <JsonView data={step.response_body} label="Response" />
          )
        )}
      </div>

      {stepEdit.confirmOpen && (
        <ConfirmModal
          title={confirmProps.title}
          body={confirmProps.body}
          confirmLabel={confirmProps.confirmLabel}
          variant={confirmProps.variant}
          onConfirm={handleConfirm}
          onCancel={stepEdit.cancelConfirm}
          isSubmitting={stepEdit.isMutating}
        />
      )}

      {(stepEdit.error || errorToastMsg) && (
        <Toast msg={stepEdit.error || errorToastMsg || ''} onDismiss={() => setErrorToastMsg(null)} />
      )}

      {toastMsg && (
        <Toast msg={toastMsg} onDismiss={() => setToastMsg(null)} />
      )}

      {modalMode === 'fork' && (
        <ForkModal
          isOpen
          onClose={() => setModalMode(null)}
          sessionId={step.session_id}
          timelineId={step.timeline_id}
          atStep={step.step_number}
        />
      )}
      {modalMode === 'replay' && (
        <ReplaySetupModal
          isOpen
          onClose={() => setModalMode(null)}
          sessionId={step.session_id}
          timelineId={step.timeline_id}
          atStep={step.step_number}
        />
      )}
      {modalMode === 'runReplay' && (
        <ReplayJobModal
          sessionId={step.session_id}
          sourceTimelineId={step.timeline_id}
          atStep={step.step_number}
          onClose={() => setModalMode(null)}
        />
      )}
    </div>
  )
}

function TabButton({ icon: Icon, label, active, onClick, editButton }: {
  icon: React.ComponentType<{ size?: number }>; label: string; active: boolean; onClick: () => void; editButton?: () => void
}) {
  return (
    <div className="flex items-center">
      <button
        onClick={onClick}
        className={cn(
          'flex items-center gap-1.5 px-4 py-2.5 text-xs font-medium border-b-2 transition-colors',
          active
            ? 'border-cyan-400 text-cyan-300'
            : 'border-transparent text-neutral-500 hover:text-neutral-300 hover:border-neutral-700'
        )}
      >
        <Icon size={13} />
        {label}
      </button>
      {editButton && active && (
        <button
          onClick={editButton}
          title={`Edit ${label}`}
          className="p-1 text-neutral-500 hover:text-cyan-300 transition-colors"
        >
          <Pencil size={12} />
        </button>
      )}
    </div>
  )
}

function StatusPill({ status }: { status: string }) {
  const styles: Record<string, string> = {
    success: 'bg-green-950 text-green-400 border-green-800',
    error: 'bg-red-950 text-red-400 border-red-800',
    pending: 'bg-amber-950 text-amber-400 border-amber-800',
  }
  return (
    <span className={cn('text-[10px] px-1.5 py-0.5 rounded border font-medium', styles[status] || 'bg-neutral-900 text-neutral-500 border-neutral-700')}>
      {status}
    </span>
  )
}

function ContextWindowView({ messages }: { messages: { role: string; content: string }[] | null }) {
  if (!messages || messages.length === 0) {
    return (
      <div className="flex items-center justify-center h-full text-neutral-500 text-sm p-4">
        No messages extracted. Check the Request tab for the raw payload.
      </div>
    )
  }

  const roleColors: Record<string, string> = {
    system: 'text-fuchsia-400 bg-fuchsia-950/30 border-fuchsia-900/50',
    user: 'text-cyan-400 bg-cyan-950/30 border-cyan-900/50',
    assistant: 'text-green-400 bg-green-950/30 border-green-900/50',
    tool: 'text-amber-400 bg-amber-950/30 border-amber-900/50',
  }

  return (
    <div className="p-4 space-y-3">
      {messages.map((msg, i) => (
        <div key={i} className={cn('rounded-lg border p-3', roleColors[msg.role] || 'bg-neutral-900 border-neutral-800 text-neutral-300')}>
          <div className="text-[10px] font-semibold uppercase tracking-wider mb-1.5 opacity-80">{msg.role}</div>
          <pre className="text-xs whitespace-pre-wrap leading-relaxed font-mono">{msg.content}</pre>
        </div>
      ))}
    </div>
  )
}

function JsonView({ data, label }: { data: unknown | null; label: string }) {
  if (!data) {
    return (
      <div className="flex items-center justify-center h-full text-neutral-500 text-sm">
        No {label.toLowerCase()} data
      </div>
    )
  }

  return (
    <div className="p-4">
      <JsonTree data={data} />
    </div>
  )
}

function JsonEditor({ text, onChange, parseError, canSave, onSave, onCancel }: {
  text: string; onChange: (v: string) => void; parseError: string | null
  canSave: boolean; onSave: () => void; onCancel: () => void
}) {
  return (
    <div className="flex flex-col h-full">
      <textarea
        value={text}
        onChange={(e) => onChange(e.target.value)}
        className="flex-1 w-full bg-neutral-950 text-neutral-200 font-mono text-xs p-4 resize-none outline-none border-none"
        spellCheck={false}
      />
      {parseError && (
        <div className="px-4 py-1.5 text-xs text-red-400 bg-red-950/30 border-t border-red-900/50">
          {parseError}
        </div>
      )}
      <div className="flex items-center gap-2 px-4 py-2 border-t border-neutral-800">
        <button
          onClick={onSave}
          disabled={!canSave}
          className={cn(
            'px-3 py-1.5 text-xs rounded-md font-medium transition-colors',
            canSave
              ? 'bg-cyan-600 hover:bg-cyan-500 text-white'
              : 'bg-neutral-800 text-neutral-600 cursor-not-allowed'
          )}
        >
          Save
        </button>
        <button
          onClick={onCancel}
          className="px-3 py-1.5 text-xs text-neutral-400 hover:text-neutral-200 border border-neutral-700 rounded-md transition-colors"
        >
          Cancel
        </button>
      </div>
    </div>
  )
}

function ConfirmModal({ title, body, confirmLabel, variant, onConfirm, onCancel, isSubmitting }: {
  title: string; body: string; confirmLabel: string; variant: 'default' | 'destructive'
  onConfirm: () => void; onCancel: () => void; isSubmitting: boolean
}) {
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60" role="dialog" aria-label={title}>
      <div className="bg-neutral-900 border border-neutral-700 rounded-xl p-6 max-w-md w-full shadow-2xl">
        <h3 className="text-base font-semibold text-neutral-100 mb-2">{title}</h3>
        <p className="text-sm text-neutral-400 mb-6">{body}</p>
        <div className="flex gap-3 justify-end">
          <button onClick={onCancel} disabled={isSubmitting} className="px-4 py-2 text-sm text-neutral-400 hover:text-neutral-200 border border-neutral-700 rounded-lg transition-colors">
            Cancel
          </button>
          <button onClick={onConfirm} disabled={isSubmitting} className={cn(
            'px-4 py-2 text-sm rounded-lg transition-colors font-medium',
            variant === 'destructive'
              ? 'bg-red-600 hover:bg-red-500 text-white'
              : 'bg-cyan-600 hover:bg-cyan-500 text-white'
          )}>
            {isSubmitting ? 'Saving…' : confirmLabel}
          </button>
        </div>
      </div>
    </div>
  )
}

function Toast({ msg, onDismiss }: { msg: string; onDismiss: () => void }) {
  useEffect(() => {
    const t = setTimeout(onDismiss, 5000)
    return () => clearTimeout(t)
  }, [onDismiss])

  return (
    <div className="fixed bottom-4 right-4 z-50 bg-cyan-950 border border-cyan-800 text-cyan-200 text-sm px-4 py-3 rounded-lg shadow-xl">
      {msg}
    </div>
  )
}

