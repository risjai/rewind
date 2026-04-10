import { create } from 'zustand'

interface UIState {
  selectedSessionId: string | null
  selectedTimelineId: string | null
  selectedStepId: string | null
  sidebarCollapsed: boolean
  view: 'sessions' | 'diff' | 'baselines' | 'evaluations'

  // Eval state
  selectedDatasetName: string | null
  selectedExperimentId: string | null
  evalTab: 'datasets' | 'experiments' | 'compare'

  selectSession: (id: string | null) => void
  selectTimeline: (id: string | null) => void
  selectStep: (id: string | null) => void
  toggleSidebar: () => void
  setView: (view: UIState['view']) => void
  selectDataset: (name: string | null) => void
  selectExperiment: (id: string | null) => void
  setEvalTab: (tab: UIState['evalTab']) => void
}

function parseHash(): { sessionId: string | null; stepId: string | null } {
  const hash = window.location.hash.slice(1)
  const parts = hash.split('/')
  return {
    sessionId: parts[1] || null,
    stepId: parts[2] || null,
  }
}

export const useStore = create<UIState>((set) => {
  const initial = parseHash()
  return {
    selectedSessionId: initial.sessionId,
    selectedTimelineId: null,
    selectedStepId: initial.stepId,
    sidebarCollapsed: false,
    view: 'sessions',

    // Eval state
    selectedDatasetName: null,
    selectedExperimentId: null,
    evalTab: 'datasets',

    selectSession: (id) => {
      set({ selectedSessionId: id, selectedStepId: null, selectedTimelineId: null, view: 'sessions' })
      window.location.hash = id ? `#/session/${id}` : ''
    },
    selectTimeline: (id) => set({ selectedTimelineId: id }),
    selectStep: (id) => set({ selectedStepId: id }),
    toggleSidebar: () => set((s) => ({ sidebarCollapsed: !s.sidebarCollapsed })),
    setView: (view) => set({ view }),
    selectDataset: (name) => set({ selectedDatasetName: name }),
    selectExperiment: (id) => set({ selectedExperimentId: id }),
    setEvalTab: (tab) => set({ evalTab: tab }),
  }
})
