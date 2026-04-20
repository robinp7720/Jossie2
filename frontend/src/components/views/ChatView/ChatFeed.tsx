import React, { useEffect, useRef } from 'react';
import type { ChatMessage } from '../../../hooks/useChat';
import { MessageItem } from './MessageItem';

interface ChatFeedProps {
  messages: ChatMessage[];
}

export const ChatFeed: React.FC<ChatFeedProps> = ({ messages }) => {
  const chatEndRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    chatEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [messages]);

  return (
    <div className="chat-feed">
      {messages.length === 0 && (
        <div className="empty-state">
          <p className="eyebrow">Blank slate</p>
          <h2>Start with a concrete job.</h2>
          <p>
            Ask Jossie to inspect mail, summarize new events, schedule follow-up work,
            or capture state into memory. This view is optimized for multi-step runs,
            not one-off prompts.
          </p>
        </div>
      )}
      {messages.map((message) => (
        <MessageItem key={message.id} message={message} />
      ))}
      <div ref={chatEndRef} />
    </div>
  );
};
