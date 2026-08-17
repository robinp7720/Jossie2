import type { PendingAction, WorkRun } from '../types'

export type RunStep = {
  id: string
  label: string
  status: 'running' | 'done' | 'error'
}

export function AgentRunStatus({
  steps,
  runs = [],
  actions,
  onDecision,
}: {
  steps: RunStep[]
  runs?: WorkRun[]
  actions: PendingAction[]
  onDecision: (action: PendingAction, approve: boolean) => void
}) {
  return (
    <>
      {(steps.length > 0 || runs.length > 0) && (
        <div
          className="run-timeline"
          aria-label="Current run"
          aria-live="polite"
        >
          <p>Jossie at work</p>
          {runs.map((run) => (
            <div
              key={run.id}
              className={`run-step ${run.status === 'waiting_for_approval' ? 'error' : 'running'}`}
            >
              <i />
              {run.current_phase || run.summary}
              <span>
                {run.status === 'waiting_for_approval'
                  ? 'Waiting for you'
                  : run.cancel_requested
                    ? 'Stopping'
                    : 'Working'}
              </span>
            </div>
          ))}
          {steps.slice(-6).map((step) => (
            <div key={step.id} className={`run-step ${step.status}`}>
              <i />
              {step.label}
              <span>
                {step.status === 'running'
                  ? 'Working'
                  : step.status === 'error'
                    ? 'Needs attention'
                    : 'Done'}
              </span>
            </div>
          ))}
        </div>
      )}
      {actions.length > 0 && (
        <div className="approval-stack">
          <p className="list-label">YOUR APPROVAL</p>
          {actions.map((action) => (
            <article
              className={`approval-card ${action.effect}`}
              key={action.id}
            >
              <div>
                <strong>{action.title}</strong>
                <span>
                  {action.effect === 'destructive'
                    ? 'Destructive action'
                    : 'External action'}
                </span>
              </div>
              <p>{action.summary}</p>
              {action.result_error && <small>{action.result_error}</small>}
              {action.status === 'pending' ? (
                <footer>
                  <button
                    className="button secondary"
                    onClick={() => onDecision(action, false)}
                  >
                    Reject
                  </button>
                  <button
                    className="button primary"
                    onClick={() => onDecision(action, true)}
                  >
                    Approve
                  </button>
                </footer>
              ) : (
                <footer>
                  <span>
                    {action.status === 'uncertain'
                      ? 'Verify this action manually before retrying.'
                      : 'Processing…'}
                  </span>
                </footer>
              )}
            </article>
          ))}
        </div>
      )}
    </>
  )
}
