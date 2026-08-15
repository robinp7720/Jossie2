import { useEffect, useMemo, useState } from 'react'
import type { ApiConfig } from '../api'
import { cancelWorkRun, controlGoal, getGoal, getWork, getWorkRun, updateGoal } from '../api'
import { useWorkspaceEvents } from '../events'
import type { Goal, GoalDetail, WorkRun, WorkRunDetail, WorkSummary } from '../types'
import { formatDate } from '../utils/format'

const statusLabel = (status: string) => status.replace(/_/g, ' ')
const isGoalOpen = (goal: Goal) => ['active', 'paused', 'blocked'].includes(goal.status)
const nextGoalTask = (goal: Goal) => goal.tasks.find((task) => task.status === 'in_progress')
  ?? goal.tasks.find((task) => ['blocked', 'waiting'].includes(task.status))
  ?? goal.tasks.find((task) => task.status === 'pending')

const betweenRunsLabel = (goal: Goal) => {
  if (goal.status === 'blocked') return 'blocked · waiting for input'
  if (goal.status === 'paused') return 'paused · ready to resume'
  return 'open goal · between runs'
}

export function WorkPage({ api }: { api: ApiConfig }) {
  const [work, setWork] = useState<WorkSummary | null>(null)
  const [selectedGoal, setSelectedGoal] = useState<GoalDetail | null>(null)
  const [selectedRun, setSelectedRun] = useState<WorkRunDetail | null>(null)
  const [editingTitle, setEditingTitle] = useState('')
  const [error, setError] = useState<string | null>(null)
  const { event, sequence } = useWorkspaceEvents()

  const refresh = async () => {
    try {
      const next = await getWork(api)
      setWork(next)
      setError(null)
      if (selectedGoal) setSelectedGoal(await getGoal(api, selectedGoal.id))
      if (selectedRun) setSelectedRun(await getWorkRun(api, selectedRun.id))
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : 'Unable to load work status.')
    }
  }

  useEffect(() => {
    void refresh()
  }, [selectedGoal?.id, selectedRun?.id])

  useEffect(() => {
    if (event && ['goal_updated', 'work_run_updated', 'work_step_updated', 'worker_status_updated', 'action_approval_required', 'action_resolved', 'background_notification'].includes(event.type)) {
      void refresh()
    }
  }, [sequence])

  const goals = useMemo(() => work?.goals.filter(isGoalOpen) ?? [], [work])
  const goalsBetweenRuns = useMemo(() => {
    const runningGoalIds = new Set(work?.active_runs.flatMap((run) => run.goal_id ? [run.goal_id] : []) ?? [])
    return goals.filter((goal) => !runningGoalIds.has(goal.id))
  }, [goals, work?.active_runs])

  const chooseGoal = async (goal: Goal) => {
    setSelectedRun(null)
    setEditingTitle(goal.title)
    setSelectedGoal(await getGoal(api, goal.id))
  }

  const chooseRun = async (run: WorkRun) => {
    setSelectedGoal(null)
    setSelectedRun(await getWorkRun(api, run.id))
  }

  const actOnGoal = async (goal: Goal, action: 'pause' | 'resume' | 'cancel') => {
    try {
      await controlGoal(api, goal.id, action)
      await refresh()
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : 'Unable to update the goal.')
    }
  }

  if (!work) return <section className="page"><div className="page-loading"><span className="brand-orb">J</span><h1>Loading current work…</h1></div></section>

  return <section className="page work-page">
    <header className="page-head"><div><p className="eyebrow">GOALS AND OPERATIONS</p><h1>Work in progress.</h1><p className="muted-copy">Outcome progress, current execution, and background health in one place.</p></div></header>
    {error && <p className="chat-action-error" role="alert">{error}</p>}
    <div className="work-summary" aria-live="polite">
      <article><strong>{work.active_runs.length}</strong><span>running now</span></article>
      <article><strong>{goals.length}</strong><span>open goals</span></article>
      <article><strong>{work.active_runs.filter((run) => run.status === 'waiting_for_approval').length}</strong><span>waiting for you</span></article>
      <article><strong>{work.workers.filter((worker) => worker.status === 'degraded').length}</strong><span>workers need attention</span></article>
    </div>

    <div className="work-grid">
      <section className="panel-new work-now">
        <div className="panel-head"><div><p className="eyebrow">NOW</p><h2>What’s happening</h2></div></div>
        <div className="work-card-list">
          {work.active_runs.map((run) => <button className="work-run-card" key={run.id} onClick={() => void chooseRun(run)}>
            <span className={`status-dot ${run.status}`} />
            <div><strong>{run.current_phase || run.summary}</strong><small>{statusLabel(run.kind)} · {statusLabel(run.status)} · started {formatDate(run.started_at || run.created_at)}</small></div>
            <span>→</span>
          </button>)}
          {goalsBetweenRuns.map((goal) => {
            const task = nextGoalTask(goal)
            return <button className="work-run-card" key={`goal-${goal.id}`} onClick={() => void chooseGoal(goal)}>
              <span className={`status-dot ${goal.status === 'active' ? 'queued' : goal.status}`} />
              <div><strong>{task?.title || goal.title}</strong><small>{goal.title} · {betweenRunsLabel(goal)} · {goal.completed_tasks} of {goal.total_tasks} complete</small></div>
              <span>→</span>
            </button>
          })}
          {!work.active_runs.length && !goalsBetweenRuns.length && <p className="empty-copy">No execution or open goal needs attention right now.</p>}
        </div>
      </section>

      <section className="panel-new work-goals">
        <div className="panel-head"><div><p className="eyebrow">GOALS</p><h2>Outcome progress</h2></div></div>
        <div className="goal-card-list">{goals.length ? goals.map((goal) => <button className="goal-card" key={goal.id} onClick={() => void chooseGoal(goal)}>
          <div><strong>{goal.title}</strong><span className={`status-pill ${goal.status}`}>{statusLabel(goal.status)}</span></div>
          <p>{goal.objective}</p>
          <footer><progress max={Math.max(goal.total_tasks, 1)} value={goal.completed_tasks} /><span>{goal.completed_tasks} of {goal.total_tasks} complete</span></footer>
        </button>) : <p className="empty-copy">Substantial requests will appear here as trackable goals.</p>}</div>
      </section>

      {(selectedGoal || selectedRun) && <section className="panel-new work-detail">
        {selectedGoal && <>
          <div className="panel-head"><div><p className="eyebrow">GOAL DETAIL</p><h2>{selectedGoal.title}</h2></div><button className="text-button" onClick={() => setSelectedGoal(null)}>Close</button></div>
          <div className="goal-edit"><input value={editingTitle} onChange={(event) => setEditingTitle(event.target.value)} /><button className="button secondary" onClick={() => void updateGoal(api, selectedGoal.id, { title: editingTitle }).then(() => refresh())}>Rename</button></div>
          <p className="muted-copy">{selectedGoal.objective}</p>
          {selectedGoal.blocker && <p className="work-error">Blocked: {selectedGoal.blocker}</p>}
          <ol className="goal-task-list">{selectedGoal.tasks.map((task) => <li key={task.id} className={task.status}><i /> <div><strong>{task.title}</strong><small>{statusLabel(task.status)}{task.summary ? ` · ${task.summary}` : ''}</small>{task.blocker && <span>{task.blocker}</span>}</div></li>)}</ol>
          <footer className="work-controls">
            {selectedGoal.status === 'active' && <button className="button primary" onClick={() => void actOnGoal(selectedGoal, 'resume')}>Continue now</button>}
            {selectedGoal.status === 'active' ? <button className="button secondary" onClick={() => void actOnGoal(selectedGoal, 'pause')}>Pause</button> : <button className="button primary" onClick={() => void actOnGoal(selectedGoal, 'resume')}>Resume</button>}
            <button className="button danger" onClick={() => void actOnGoal(selectedGoal, 'cancel')}>Cancel goal</button>
            <button className="text-button" onClick={() => void updateGoal(api, selectedGoal.id, { archived: true }).then(() => { setSelectedGoal(null); return refresh() })}>Archive</button>
          </footer>
        </>}
        {selectedRun && <>
          <div className="panel-head"><div><p className="eyebrow">RUN DETAIL</p><h2>{selectedRun.current_phase || selectedRun.summary}</h2></div><button className="text-button" onClick={() => setSelectedRun(null)}>Close</button></div>
          <p className="muted-copy">{statusLabel(selectedRun.kind)} · {statusLabel(selectedRun.status)} · updated {formatDate(selectedRun.updated_at)}</p>
          {selectedRun.error && <p className="work-error">{selectedRun.error}</p>}
          <ol className="goal-task-list">{selectedRun.steps.map((step) => <li key={step.id} className={step.status}><i /><div><strong>{step.label}</strong><small>{statusLabel(step.status)}{step.summary ? ` · ${step.summary}` : ''}</small></div></li>)}</ol>
          {selectedRun.status === 'paused' && selectedRun.goal_id && <footer className="work-controls"><button className="button primary" onClick={() => void controlGoal(api, selectedRun.goal_id!, 'resume').then(() => { setSelectedRun(null); return refresh() })}>Resume from checkpoint</button></footer>}
          {['queued', 'running', 'waiting_for_approval'].includes(selectedRun.status) && <footer className="work-controls"><button className="button danger" onClick={() => void cancelWorkRun(api, selectedRun.id).then(() => refresh())}>Stop run</button></footer>}
        </>}
      </section>}

      <section className="panel-new work-background">
        <div className="panel-head"><div><p className="eyebrow">BACKGROUND</p><h2>Operational health</h2></div><small>Routine successful checks are aggregated</small></div>
        {(work.scheduled_tasks.length > 0 || work.chat_imports.some((item) => ['queued', 'processing'].includes(item.status))) && <div className="operational-list">
          {work.scheduled_tasks.map((task) => <article key={task.id}><span className={`status-dot ${task.status}`} /><div><strong>Scheduled {statusLabel(task.task_type)}</strong><small>{statusLabel(task.schedule_type)} · next {formatDate(task.next_run_at)} · {task.run_count || 0} runs</small>{task.last_error && <em>{task.last_error}</em>}</div></article>)}
          {work.chat_imports.filter((item) => ['queued', 'processing'].includes(item.status)).map((item) => <article key={item.id}><span className={`status-dot ${item.status === 'processing' ? 'running' : item.status}`} /><div><strong>Chat export import</strong><small>{statusLabel(item.status)} · {item.analyzed_messages} of {item.total_messages || '?'} messages selected</small>{item.error && <em>{item.error}</em>}</div></article>)}
        </div>}
        <div className="worker-list">{work.workers.map((worker) => <article key={worker.worker_key}>
          <span className={`status-dot ${worker.status}`} /><div><strong>{worker.label}</strong><small>{worker.detail || statusLabel(worker.status)} · last healthy {formatDate(worker.last_success_at)}</small>{worker.last_error && <em>{worker.last_error}</em>}</div><span className={`status-pill ${worker.status}`}>{statusLabel(worker.status)}</span>
        </article>)}</div>
      </section>

      <section className="panel-new work-history">
        <div className="panel-head"><div><p className="eyebrow">HISTORY</p><h2>Significant recent work</h2></div></div>
        <div className="work-card-list">{work.recent_runs.filter((run) => !work.active_runs.some((active) => active.id === run.id)).map((run) => <button className="work-run-card" key={run.id} onClick={() => void chooseRun(run)}><span className={`status-dot ${run.status}`} /><div><strong>{run.summary}</strong><small>{statusLabel(run.status)} · {formatDate(run.updated_at)}</small></div><span>→</span></button>)}</div>
      </section>
    </div>
  </section>
}
