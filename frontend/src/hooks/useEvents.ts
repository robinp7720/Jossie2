import { useEffect } from 'react';
import { useAppContext } from '../context/AppContext';
import { buildWebSocketUrl } from '../api';
import type { Message, Conversation } from '../types';

type ServerEvent = {
  type: string;
  conversation_id?: string;
  run_id?: string;
  iteration?: number;
  content?: string;
  reason?: string;
  tool?: string;
  call_id?: string;
  arguments_preview?: string;
  result_preview?: string;
  is_error?: boolean;
  feedback?: string;
  title?: string | null;
  updated_at?: string;
  source?: string;
  message?: Message | string;
  error?: string;
};

export const useEvents = () => {
  const { 
    apiConfig, 
    canConnect, 
    setConversations, 
    addActivity, 
    setStatusMessage,
    activeConversationId 
  } = useAppContext();

  useEffect(() => {
    if (!canConnect) return;

    const ws = new WebSocket(buildWebSocketUrl(apiConfig, '/api/events'));

    ws.onmessage = (event) => {
      try {
        const payload = JSON.parse(event.data) as ServerEvent;

        if (payload.type === 'conversation_updated' && payload.conversation_id) {
          setConversations((prev) => {
            const existing = prev.find((c) => c.id === payload.conversation_id);
            const nextConversation: Conversation = existing
              ? {
                  ...existing,
                  title: payload.title ?? existing.title,
                  updated_at: payload.updated_at ?? existing.updated_at,
                }
              : {
                  id: payload.conversation_id!,
                  title: payload.title ?? null,
                  created_at: payload.updated_at ?? new Date().toISOString(),
                  updated_at: payload.updated_at ?? new Date().toISOString(),
                };

            const next = [
              nextConversation,
              ...prev.filter((c) => c.id !== payload.conversation_id),
            ];
            next.sort(
              (left, right) =>
                new Date(right.updated_at).getTime() - new Date(left.updated_at).getTime()
            );
            return next;
          });
          return;
        }

        if (payload.type === 'background_notification' && payload.conversation_id) {
          const detail = typeof payload.message === 'string' 
            ? payload.message 
            : payload.content ?? '';
            
          addActivity({
            conversationId: payload.conversation_id,
            label: payload.source ? `${payload.source} update` : 'Background update',
            detail,
            tone: 'success',
          });
          return;
        }

        if (payload.type === 'cancel_requested' && payload.conversation_id) {
          addActivity({
            conversationId: payload.conversation_id,
            label: 'Cancel requested',
            detail: 'The current run will stop at the next safe checkpoint.',
            tone: 'warn',
          });
          return;
        }
        
        // Note: message_created is handled by useChat when it's the active conversation
        // but we might want to log it here too if it's NOT the active one.
        if (payload.type === 'message_created' && payload.conversation_id !== activeConversationId) {
            // Optional: notify user about messages in other conversations
        }

      } catch (error) {
        console.error('Failed to parse event:', error);
      }
    };

    ws.onerror = () => {
      setStatusMessage('Background events connection failed.');
    };

    return () => {
      ws.close();
    };
  }, [apiConfig, canConnect, setConversations, addActivity, setStatusMessage, activeConversationId]);
};
