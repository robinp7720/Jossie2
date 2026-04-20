import React, { useState, useMemo, useRef, useEffect } from 'react';
import { Chip, Button } from '../../common/UI';
import type { ChatMessage } from '../../../hooks/useChat';

interface ComposerProps {
  messages: ChatMessage[];
  onSend: (content: string) => void;
  onCancel: () => void;
  onFileUpload: (file: File) => void;
  isSending: boolean;
  isStreaming: boolean;
  onToggleStreaming: (val: boolean) => void;
  uploading: boolean;
  pendingFileIds: string[];
}

export const Composer: React.FC<ComposerProps> = ({
  messages,
  onSend,
  onCancel,
  onFileUpload,
  isSending,
  isStreaming,
  onToggleStreaming,
  uploading,
  pendingFileIds,
}) => {
  const [composer, setComposer] = useState('');
  const [isDragging, setIsDragging] = useState(false);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    const textarea = textareaRef.current;
    if (textarea) {
      textarea.style.height = 'auto';
      textarea.style.height = `${Math.min(textarea.scrollHeight, 300)}px`;
    }
  }, [composer]);

  const suggestionPrompts = useMemo(() => {
    if (messages.length === 0) {
      return [
        'Summarize my current setup and what you can do for me.',
        'Check for anything new that needs my attention.',
        'Help me create a recurring review workflow.',
      ];
    }

    const lastAssistant = [...messages]
      .reverse()
      .find((message) => message.role === 'assistant');

    if (!lastAssistant) {
      return [
        'Summarize the latest activity.',
        'What should I do next?',
        'Turn this into a scheduled follow-up.',
      ];
    }

    return [
      'Summarize this thread in three bullets.',
      'List any follow-ups or reminders I should create.',
      'Save the important facts from this thread to memory.',
    ];
  }, [messages]);

  const handleSend = () => {
    if (composer.trim()) {
      onSend(composer);
      setComposer('');
    }
  };

  const handleKeyDown = (event: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key === 'Enter' && !event.shiftKey) {
      event.preventDefault();
      handleSend();
    }
  };

  const handleFileChange = (event: React.ChangeEvent<HTMLInputElement>) => {
    const files = event.target.files;
    if (!files || files.length === 0) return;
    for (const file of Array.from(files)) {
      onFileUpload(file);
    }
    event.target.value = '';
  };

  const onDragOver = (e: React.DragEvent) => {
    e.preventDefault();
    setIsDragging(true);
  };

  const onDragLeave = () => {
    setIsDragging(false);
  };

  const onDrop = (e: React.DragEvent) => {
    e.preventDefault();
    setIsDragging(false);
    const files = e.dataTransfer.files;
    if (files && files.length > 0) {
      for (const file of Array.from(files)) {
        onFileUpload(file);
      }
    }
  };

  return (
    <div
      className={`composer ${isDragging ? 'dragging' : ''}`}
      onDragOver={onDragOver}
      onDragLeave={onDragLeave}
      onDrop={onDrop}
    >
      <div className="composer-head">
        <div>
          <p className="eyebrow">Command deck</p>
          <h3>Dispatch a new run</h3>
        </div>
        <div className="composer-mode">
          <label className="toggle">
            <input
              type="checkbox"
              checked={isStreaming}
              onChange={(event) => onToggleStreaming(event.target.checked)}
            />
            <span>{isStreaming ? 'Streaming' : 'Standard response'}</span>
          </label>
        </div>
      </div>

      <textarea
        ref={textareaRef}
        placeholder="Describe the job, context, and any follow-up you want Jossie to handle."
        value={composer}
        onChange={(event) => setComposer(event.target.value)}
        onKeyDown={handleKeyDown}
        rows={1}
      />

      {suggestionPrompts.length > 0 && !composer && (
        <div className="quick-actions">
          {suggestionPrompts.map((prompt) => (
            <Chip
              key={prompt}
              variant="neutral"
              className="action-chip"
              onClick={() => setComposer(prompt)}
            >
              {prompt}
            </Chip>
          ))}
        </div>
      )}

      {pendingFileIds.length > 0 && (
        <div className="pending-files">
          {pendingFileIds.map((id) => (
            <Chip key={id} variant="accent" className="file-chip">
              File {id.slice(0, 8)}
            </Chip>
          ))}
          {uploading && <span className="muted">Uploading...</span>}
        </div>
      )}

      <div className="composer-actions">
        <div className="composer-left">
          <label className="button ghost file-upload-label">
            {uploading ? 'Uploading…' : 'Attach files'}
            <input
              type="file"
              multiple
              onChange={handleFileChange}
              style={{ display: 'none' }}
            />
          </label>
          <span className="composer-hint">Enter to send, Shift+Enter for a new line.</span>
        </div>
        <div className="composer-button-row">
          <Button variant="ghost" onClick={onCancel} disabled={!isSending}>
            Cancel
          </Button>
          <Button
            variant="primary"
            onClick={handleSend}
            loading={isSending}
            disabled={isSending || (!composer.trim() && pendingFileIds.length === 0)}
          >
            Send
          </Button>
        </div>
      </div>
      {isDragging && (
        <div className="drag-overlay">
          <p>Drop files to add them to this run</p>
        </div>
      )}
    </div>
  );
};
