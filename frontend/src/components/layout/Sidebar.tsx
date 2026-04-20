import React from 'react';
import { useAppContext } from '../../context/AppContext';
import { Button, Chip } from '../common/UI';

const formatRelativeTime = (value?: string) => {
  if (!value) return '';
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  const diffMs = date.getTime() - Date.now();
  const diffMinutes = Math.round(diffMs / 60000);
  const formatter = new Intl.RelativeTimeFormat(undefined, { numeric: 'auto' });
  if (Math.abs(diffMinutes) < 60) return formatter.format(diffMinutes, 'minute');
  const diffHours = Math.round(diffMinutes / 60);
  if (Math.abs(diffHours) < 48) return formatter.format(diffHours, 'hour');
  const diffDays = Math.round(diffHours / 24);
  return formatter.format(diffDays, 'day');
};

interface SidebarProps {
  activeTab: string;
  onTabChange: (tab: 'assistant' | 'integrations' | 'accounts' | 'knowledge') => void;
}

export const Sidebar: React.FC<SidebarProps> = ({ activeTab, onTabChange }) => {
  const {
    conversations,
    activeConversationId,
    setActiveConversationId, 
    refreshConversations,
    canConnect,
    statusMessage 
  } = useAppContext();

  const tabs: Array<{
    id: 'assistant' | 'integrations' | 'accounts' | 'knowledge';
    label: string;
    description: string;
  }> = [
    {
      id: 'assistant',
      label: 'Assistant',
      description: 'Runs, conversation history, live replies',
    },
    {
      id: 'integrations',
      label: 'Integrations',
      description: 'Connection health and onboarding',
    },
    {
      id: 'accounts',
      label: 'Accounts',
      description: 'Stored credentials and identities',
    },
    {
      id: 'knowledge',
      label: 'Knowledge',
      description: 'Graph memory and entity links',
    },
  ];

  const handleStartNewConversation = () => {
    setActiveConversationId(null);
    onTabChange('assistant');
  };

  return (
    <aside className="sidebar">
      <div className="sidebar-top">
        <div className="brand">
          <div className="brand-mark">J</div>
          <div>
            <p className="brand-title">Jossie</p>
            <p className="brand-subtitle">Ops cockpit for the agent runtime</p>
          </div>
        </div>

        <div className="sidebar-summary">
          <div>
            <span className="summary-label">Conversations</span>
            <strong>{conversations.length}</strong>
          </div>
          <div>
            <span className="summary-label">Status</span>
            <strong>{canConnect ? 'Connected' : 'Offline'}</strong>
          </div>
        </div>
      </div>

      <nav className="tabs" aria-label="Primary">
        {tabs.map((tab) => (
          <button
            key={tab.id}
            className={activeTab === tab.id ? 'tab active' : 'tab'}
            onClick={() => onTabChange(tab.id)}
          >
            <span className="tab-title">{tab.label}</span>
            <span className="tab-description">{tab.description}</span>
          </button>
        ))}
      </nav>

      <div className="conversation-panel">
        <div className="panel-header">
          <div>
            <p className="eyebrow">Threads</p>
            <h3>Recent conversations</h3>
          </div>
          <Button variant="primary" size="sm" onClick={handleStartNewConversation}>
            New thread
          </Button>
        </div>
        <div className="conversation-list">
          {conversations.length === 0 && (
            <div className="empty-panel">
              <p>No conversations yet.</p>
              <span>Start a fresh thread to begin using the assistant.</span>
            </div>
          )}
          {conversations.map((conversation, index) => (
            <button
              key={conversation.id}
              className={
                conversation.id === activeConversationId
                  ? 'conversation active'
                  : 'conversation'
              }
              style={{ ['--delay' as any]: `${index * 40}ms` }}
              onClick={() => {
                setActiveConversationId(conversation.id);
                onTabChange('assistant');
              }}
            >
              <span className="conversation-header-row">
                <span className="conversation-title">
                  {conversation.title ?? 'Untitled conversation'}
                </span>
                <span className="conversation-meta">
                  {formatRelativeTime(conversation.updated_at)}
                </span>
              </span>
              <span className="conversation-preview">
                Conversation ID {conversation.id.slice(0, 8)}
              </span>
            </button>
          ))}
        </div>
      </div>

      <div className="status-card">
        <div className="status-card-header">
          <div>
            <p className="status-title">Control link</p>
            <p className={canConnect ? 'status-ok' : 'status-warn'}>
              {canConnect ? 'API reachable' : 'Token required'}
            </p>
          </div>
          <Chip variant={canConnect ? 'success' : 'warning'}>
            {canConnect ? 'Live' : 'Needs auth'}
          </Chip>
        </div>
        <p className="status-message">
          {statusMessage ??
            (canConnect
              ? 'The workspace can refresh conversations, subscribe to events, and send runs.'
              : 'Set a base URL and auth token in the inspector to unlock the workspace.')}
        </p>
        {!canConnect && (
          <Button variant="ghost" size="sm" onClick={() => refreshConversations()}>
            Retry connection
          </Button>
        )}
      </div>
    </aside>
  );
};
