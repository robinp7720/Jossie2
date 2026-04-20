import React from 'react';
import { useAppContext } from '../../../context/AppContext';
import { useChat } from '../../../hooks/useChat';
import { ChatFeed } from './ChatFeed';
import { Composer } from './Composer';
import { Chip } from '../../common/UI';

export const ChatView: React.FC = () => {
  const { apiConfig, activeConversationId } = useAppContext();
  const {
    messages,
    isSending,
    isStreaming,
    setIsStreaming,
    uploading,
    pendingFileIds,
    uploadFile,
    sendMessage,
    cancelRun,
  } = useChat();

  const userMessages = messages.filter((message) => message.role === 'user').length;
  const assistantMessages = messages.filter((message) => message.role === 'assistant').length;

  return (
    <section className="view assistant-view">
      <header className="hero hero-chat">
        <div className="hero-copy">
          <p className="eyebrow">Assistant workspace</p>
          <h1>Operate the agent, not just the prompt box.</h1>
          <p className="hero-text">
            Jossie can stream replies, call tools, keep thread state, and surface
            background work in the same interface.
          </p>
        </div>
        <div className="hero-rail">
          <div className="chip-row wrap">
            <Chip variant={isStreaming ? 'success' : 'neutral'}>
              {isStreaming ? 'Streaming enabled' : 'Streaming paused'}
            </Chip>
            <Chip variant={isSending ? 'accent' : 'neutral'}>
              {isSending ? 'Run in progress' : 'Ready for input'}
            </Chip>
            <Chip variant="neutral">
              {activeConversationId ? `Thread ${activeConversationId.slice(0, 8)}` : 'New thread'}
            </Chip>
          </div>
          <div className="stats-strip">
            <div className="stat-tile">
              <span className="stat-label">Messages</span>
              <strong>{messages.length}</strong>
            </div>
            <div className="stat-tile">
              <span className="stat-label">User turns</span>
              <strong>{userMessages}</strong>
            </div>
            <div className="stat-tile">
              <span className="stat-label">Assistant turns</span>
              <strong>{assistantMessages}</strong>
            </div>
          </div>
        </div>
      </header>

      <div className="chat-shell panel-surface">
        <div className="chat-stage-head">
          <div>
            <p className="eyebrow">Conversation</p>
            <h2>Live thread</h2>
          </div>
          <p className="support-copy">Endpoint: {apiConfig.baseUrl || 'not configured'}</p>
        </div>
        <ChatFeed messages={messages} />
        <Composer
          messages={messages}
          onSend={sendMessage}
          onCancel={cancelRun}
          onFileUpload={uploadFile}
          isSending={isSending}
          isStreaming={isStreaming}
          onToggleStreaming={setIsStreaming}
          uploading={uploading}
          pendingFileIds={pendingFileIds}
        />
      </div>
    </section>
  );
};
