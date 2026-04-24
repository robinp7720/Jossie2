import React from 'react';
import type { RunStatus } from '../../../hooks/useChat';

interface RunStatusPanelProps {
  status: RunStatus;
}

export const RunStatusPanel: React.FC<RunStatusPanelProps> = ({ status }) => {
  if (!status.active && status.steps.length === 0) return null;

  const latestSteps = status.steps.slice(0, 4);

  return (
    <section className={`run-status ${status.active ? 'active' : 'idle'}`}>
      <div className="run-status-main">
        <div>
          <p className="eyebrow">{status.active ? 'Active run' : 'Latest run'}</p>
          <h3>{status.phase}</h3>
        </div>
        <span className={`run-status-pill ${status.active ? 'live' : 'done'}`}>
          {status.active ? 'Live' : 'Complete'}
        </span>
      </div>
      <p className="run-status-detail">{status.detail}</p>
      <div className="run-status-meta">
        {status.startedAt ? <span>Started {status.startedAt}</span> : null}
        {status.runId ? <span>Run {status.runId.slice(0, 8)}</span> : null}
        {typeof status.iteration === 'number' ? <span>Pass {status.iteration + 1}</span> : null}
      </div>
      {latestSteps.length > 0 ? (
        <ol className="run-status-steps">
          {latestSteps.map((step) => (
            <li key={step.id} className={step.tone ? `tone-${step.tone}` : undefined}>
              <span>{step.label}</span>
              <p>{step.detail}</p>
              <time>{step.at}</time>
            </li>
          ))}
        </ol>
      ) : null}
    </section>
  );
};
