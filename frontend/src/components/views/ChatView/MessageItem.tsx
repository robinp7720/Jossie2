import React from 'react';
import ReactMarkdown from 'react-markdown';
import type { ChatMessage } from '../../../hooks/useChat';

const formatClockTime = (value?: string) => {
  if (!value) return '';
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return new Intl.DateTimeFormat(undefined, {
    hour: '2-digit',
    minute: '2-digit',
  }).format(date);
};

const prettifyContent = (content: string) => {
  try {
    const parsed = JSON.parse(content);
    return JSON.stringify(parsed, null, 2);
  } catch {
    return content;
  }
};

interface MessageItemProps {
  message: ChatMessage;
}

export const MessageItem: React.FC<MessageItemProps> = ({ message }) => {
  const isAssistant = message.role === 'assistant';
  const isUser = message.role === 'user';
  const isTool = message.role === 'tool';
  const roleLabel = isAssistant
    ? 'Assistant'
    : isUser
      ? 'User'
      : isTool
        ? 'Tool'
        : 'System';

  return (
    <article className={`message ${message.role} ${message.pending ? 'pending' : ''}`}>
      <div className="message-chrome">
        <div className="message-role">
          <span className="message-badge">{roleLabel.slice(0, 1)}</span>
          <div>
            <span className="message-role-label">
              {message.name ? `${roleLabel} · ${message.name}` : roleLabel}
            </span>
            {message.toolCallId ? (
              <span className="message-submeta">Call {message.toolCallId.slice(0, 8)}</span>
            ) : null}
          </div>
        </div>
        {message.created_at && (
          <div className="message-meta">
            {formatClockTime(message.created_at)}
          </div>
        )}
      </div>

      <div className={`message-content markdown ${isTool ? 'tool-markdown' : ''}`}>
        <ReactMarkdown>{prettifyContent(message.content)}</ReactMarkdown>
      </div>

      {message.attachments && message.attachments.length > 0 && (
        <div className="message-attachments">
          {message.attachments.map((file) => (
            <div key={file.id} className="attachment-chip">
              <span className="attachment-label">{file.name}</span>
              <span className="attachment-size">{(file.size / 1024).toFixed(1)} KB</span>
            </div>
          ))}
        </div>
      )}

      {message.toolCalls?.length ? (
        <div className="tool-call-list">
          {message.toolCalls.map((toolCall) => (
            <div key={toolCall.id} className="tool-call">
              <div className="tool-call-name">{toolCall.name}</div>
              <pre className="tool-call-args">{toolCall.arguments}</pre>
            </div>
          ))}
        </div>
      ) : null}

      {message.pending && isAssistant && (
        <div className="typing-indicator" aria-label="Assistant is typing">
          <span />
          <span />
          <span />
        </div>
      )}
    </article>
  );
};
