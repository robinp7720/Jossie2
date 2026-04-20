import React, { useMemo } from 'react';
import { useAppContext } from '../../context/AppContext';
import { Card, Chip, Button } from '../common/UI';

export const Inspector: React.FC = () => {
  const { 
    apiConfig, 
    setApiConfig, 
    refreshConversations, 
    activity, 
    activeConversationId 
  } = useAppContext();

  const visibleActivity = useMemo(() => {
    if (!activeConversationId) return activity;
    return activity.filter(
      (item) => !item.conversationId || item.conversationId === activeConversationId
    );
  }, [activity, activeConversationId]);

  return (
    <aside className="inspector">
      <Card
        eyebrow="Connection"
        title="API control plane"
        subtitle="The browser UI talks directly to the Jossie server for chat, events, and configuration."
      >
        <div className="form">
          <label>
            Base URL
            <input
              value={apiConfig.baseUrl}
              onChange={(event) =>
                setApiConfig({ ...apiConfig, baseUrl: event.target.value })
              }
              placeholder="http://localhost:8080"
            />
          </label>
          <label>
            Bearer token
            <input
              type="password"
              value={apiConfig.token}
              onChange={(event) =>
                setApiConfig({ ...apiConfig, token: event.target.value })
              }
              placeholder="Paste auth token"
            />
          </label>
          <div className="inline-actions">
            <Button variant="primary" size="sm" onClick={refreshConversations}>
              Validate
            </Button>
            <Chip variant="accent">WebSocket + REST</Chip>
          </div>
        </div>
      </Card>

      <Card
        eyebrow="Telemetry"
        title="Live activity"
        subtitle="Recent run milestones and background events for the current conversation."
        headerActions={<span className="card-meta">{visibleActivity.length} events</span>}
      >
        <div className="activity">
          {visibleActivity.length === 0 && (
            <div className="empty-panel compact">
              <p>No live activity yet.</p>
              <span>Run the assistant or wait for a background notification.</span>
            </div>
          )}
          {visibleActivity.map((event) => (
            <div
              key={event.id}
              className={`activity-item ${event.tone ? `tone-${event.tone}` : ''}`}
            >
              <div className="activity-head">
                <p className="activity-tool">{event.label}</p>
                <p className="activity-time">{event.at}</p>
              </div>
              <p className="activity-result">{event.detail}</p>
            </div>
          ))}
        </div>
      </Card>

      <Card
        eyebrow="Runtime"
        title="Operational model"
        subtitle="This frontend is designed around long-running agent workflows rather than single-shot prompt/response chat."
        tone="accent"
      >
        <div className="chip-row">
          <Chip variant="accent">Live runs</Chip>
          <Chip variant="accent">Schedules</Chip>
          <Chip variant="accent">Background events</Chip>
          <Chip variant="accent">Tool traces</Chip>
        </div>
        <p className="support-copy">
          Active conversation:
          <strong>{activeConversationId ? ` ${activeConversationId.slice(0, 8)}` : ' none selected'}</strong>
        </p>
      </Card>
    </aside>
  );
};
