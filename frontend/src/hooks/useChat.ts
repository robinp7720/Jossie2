import { useState, useCallback, useRef, useEffect } from 'react';
import { useAppContext } from '../context/AppContext';
import { 
    getMessages, 
    sendMessage as sendNonStreaming, 
    buildWebSocketUrl, 
    cancelConversation,
    uploadFile as uploadApi
} from '../api';
import type { Message, ToolCall, FileAttachment } from '../types';

export type ChatMessage = {
  id: string;
  role: 'user' | 'assistant' | 'tool' | 'system';
  content: string;
  created_at?: string;
  pending?: boolean;
  name?: string | null;
  toolCallId?: string | null;
  toolCalls?: ToolCall[];
  attachments?: FileAttachment[];
};

export type RunStatusStep = {
  id: string;
  label: string;
  detail: string;
  at: string;
  tone?: 'normal' | 'success' | 'warn';
};

export type RunStatus = {
  active: boolean;
  phase: string;
  detail: string;
  runId?: string;
  conversationId?: string;
  iteration?: number;
  startedAt?: string;
  steps: RunStatusStep[];
};

type CurrentRun = {
  conversationId?: string;
  runId?: string;
  pendingAssistantId?: string | null;
};

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
    message?: Message;
    error?: string;
};

const toChatMessage = (message: Message): ChatMessage => ({
  id: message.id,
  role: message.role,
  content: message.content,
  created_at: message.created_at,
  name: message.name ?? null,
  toolCallId: message.tool_call_id ?? null,
  toolCalls: message.tool_calls ?? undefined,
  attachments: message.attachments ?? undefined,
});

const toChatMessages = (messages: Message[]) => messages.map(toChatMessage);

const createLocalId = () => `local-${Math.random().toString(36).slice(2, 10)}`;
const createStepId = () => `step-${Math.random().toString(36).slice(2, 10)}`;

const createStep = (
  label: string,
  detail: string,
  tone: RunStatusStep['tone'] = 'normal',
): RunStatusStep => ({
  id: createStepId(),
  label,
  detail,
  tone,
  at: new Date().toLocaleTimeString(),
});

const idleRunStatus: RunStatus = {
  active: false,
  phase: 'Ready',
  detail: 'No run is active.',
  steps: [],
};

export const useChat = () => {
  const { 
    apiConfig, 
    canConnect, 
    activeConversationId, 
    setActiveConversationId,
    refreshConversations,
    addActivity,
    setStatusMessage
  } = useAppContext();

  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [isSending, setIsSending] = useState(false);
  const [isStreaming, setIsStreaming] = useState(true);
  const [uploading, setUploading] = useState(false);
  const [pendingFileIds, setPendingFileIds] = useState<string[]>([]);
  const [runStatus, setRunStatus] = useState<RunStatus>(idleRunStatus);
  
  const currentRunRef = useRef<CurrentRun | null>(null);
  const activeConversationRef = useRef<string | null>(null);

  useEffect(() => {
    activeConversationRef.current = activeConversationId;
    if (activeConversationId) {
        refreshMessages(activeConversationId);
    } else {
        setMessages([]);
    }
  }, [activeConversationId]);

  const refreshMessages = useCallback(async (conversationId: string) => {
    if (!canConnect) return;
    try {
      const data = await getMessages(apiConfig, conversationId, 100);
      setMessages(toChatMessages(data));
    } catch (error) {
      setStatusMessage(error instanceof Error ? error.message : String(error));
    }
  }, [apiConfig, canConnect, setStatusMessage]);

  const ensurePendingAssistant = useCallback((conversationId?: string) => {
    if (currentRunRef.current?.pendingAssistantId) {
      return currentRunRef.current.pendingAssistantId;
    }

    const assistantId = createLocalId();
    currentRunRef.current = {
      ...currentRunRef.current,
      conversationId,
      pendingAssistantId: assistantId,
    };
    setMessages((prev) => [
      ...prev,
      { id: assistantId, role: 'assistant', content: '', pending: true },
    ]);
    return assistantId;
  }, []);

  const updateRunStatus = useCallback(
    (
      update: Partial<Omit<RunStatus, 'steps'>>,
      step?: Omit<RunStatusStep, 'id' | 'at'>,
    ) => {
      setRunStatus((prev) => ({
        ...prev,
        ...update,
        steps: step
          ? [createStep(step.label, step.detail, step.tone), ...prev.steps].slice(0, 8)
          : prev.steps,
      }));
    },
    [],
  );

  const uploadFile = useCallback(async (file: File) => {
    if (!canConnect) return;
    setUploading(true);
    try {
        const res = await uploadApi(apiConfig, file);
        setPendingFileIds((prev) => [...prev, res.file_id]);
        addActivity({
            label: 'File uploaded',
            detail: `Shared ${res.name}`,
            tone: 'success',
        });
    } catch (error) {
        setStatusMessage(`Failed to upload ${file.name}: ${error instanceof Error ? error.message : String(error)}`);
    } finally {
        setUploading(false);
    }
  }, [apiConfig, canConnect, addActivity, setStatusMessage]);

  const sendMessage = useCallback(async (content: string) => {
    const trimmed = content.trim();
    if (!trimmed || !canConnect || isSending) return;

    setStatusMessage(null);
    setIsSending(true);
    setRunStatus({
      active: true,
      phase: isStreaming ? 'Connecting' : 'Waiting for response',
      detail: isStreaming
        ? 'Opening the live run channel.'
        : 'Sending this turn through the standard request path.',
      startedAt: new Date().toLocaleTimeString(),
      conversationId: activeConversationId ?? undefined,
      steps: [createStep('Queued request', trimmed)],
    });

    const userMessage: ChatMessage = {
      id: createLocalId(),
      role: 'user',
      content: trimmed,
    };

    const fileIds = [...pendingFileIds];
    setPendingFileIds([]);
    setMessages((prev) => [...prev, userMessage]);

    if (!isStreaming) {
      try {
        const result = await sendNonStreaming(apiConfig, trimmed, activeConversationId, fileIds);
        const assistantMessage: ChatMessage = {
          id: createLocalId(),
          role: 'assistant',
          content: result.message,
        };
        setMessages((prev) => [...prev, assistantMessage]);
        setActiveConversationId(result.conversation_id);
        refreshConversations();
        updateRunStatus(
          {
            active: false,
            phase: 'Run complete',
            detail: 'The assistant reply has been saved.',
            conversationId: result.conversation_id,
          },
          { label: 'Reply saved', detail: result.message.slice(0, 180), tone: 'success' },
        );
      } catch (error) {
        setStatusMessage(error instanceof Error ? error.message : String(error));
        updateRunStatus(
          {
            active: false,
            phase: 'Run failed',
            detail: error instanceof Error ? error.message : String(error),
          },
          {
            label: 'Request failed',
            detail: error instanceof Error ? error.message : String(error),
            tone: 'warn',
          },
        );
      } finally {
        setIsSending(false);
      }
      return;
    }

    // Streaming logic
    const ws = new WebSocket(buildWebSocketUrl(apiConfig, '/api/chat/stream'));

    ws.onopen = () => {
      ws.send(
        JSON.stringify({
          message: trimmed,
          ...(activeConversationId ? { conversation_id: activeConversationId } : {}),
          ...(fileIds.length > 0 ? { file_ids: fileIds } : {}),
        }),
      );
    };

    ws.onmessage = (event) => {
      try {
        const payload = JSON.parse(event.data) as ServerEvent;

        if (payload.type === 'run_started' && payload.conversation_id) {
          currentRunRef.current = {
            ...currentRunRef.current,
            runId: payload.run_id,
            conversationId: payload.conversation_id,
          };
          setActiveConversationId(payload.conversation_id);
          updateRunStatus(
            {
              active: true,
              phase: 'Run started',
              detail: 'Jossie is preparing the first step.',
              runId: payload.run_id,
              conversationId: payload.conversation_id,
              startedAt: new Date().toLocaleTimeString(),
            },
            { label: 'Run started', detail: `Thread ${payload.conversation_id.slice(0, 8)}` },
          );
          addActivity({
            conversationId: payload.conversation_id,
            label: 'Run started',
            detail: 'The assistant has started working on this request.',
          });
          return;
        }

        if (payload.type === 'assistant_thinking' && payload.conversation_id) {
          ensurePendingAssistant(payload.conversation_id);
          const pass = Number(payload.iteration ?? 0) + 1;
          updateRunStatus(
            {
              active: true,
              phase: `Thinking pass ${pass}`,
              detail:
                pass === 1 ? 'Planning the next step.' : 'Reviewing the latest tool results.',
              iteration: payload.iteration,
              conversationId: payload.conversation_id,
              runId: payload.run_id,
            },
            {
              label: `Thinking pass ${pass}`,
              detail: pass === 1 ? 'Planning the next step.' : 'Reviewing tool output.',
            },
          );
          addActivity({
            conversationId: payload.conversation_id,
            label: `Thinking pass ${Number(payload.iteration ?? 0) + 1}`,
            detail: 'Planning the next step.',
          });
          return;
        }

        if (payload.type === 'assistant_delta') {
          const pendingId = ensurePendingAssistant(payload.conversation_id);
          const delta = String(payload.content ?? '');
          if (delta.trim()) {
            setRunStatus((prev) => {
              if (prev.active && prev.phase === 'Writing response') return prev;
              return {
                ...prev,
                active: true,
                phase: 'Writing response',
                detail: 'Streaming the answer into the thread.',
                conversationId: payload.conversation_id,
                runId: payload.run_id,
              };
            });
          }
          setMessages((prev) =>
            prev.map((item) =>
              item.id === pendingId ? { ...item, content: `${item.content}${delta}` } : item,
            ),
          );
          return;
        }

        if (payload.type === 'assistant_reset') {
          updateRunStatus(
            {
              active: true,
              phase: payload.reason === 'reflection_retry' ? 'Revising response' : 'Changing approach',
              detail:
                payload.reason === 'reflection_retry'
                  ? 'The draft did not pass the quality check, so Jossie is rewriting it.'
                  : 'The previous step was reset before continuing.',
              conversationId: payload.conversation_id,
              runId: payload.run_id,
            },
            { label: 'Draft reset', detail: payload.reason ?? 'Assistant reset', tone: 'warn' },
          );
          const pendingId = currentRunRef.current?.pendingAssistantId;
          if (pendingId) {
            setMessages((prev) =>
              prev.map((item) =>
                item.id === pendingId ? { ...item, content: '', pending: true } : item,
              ),
            );
          }
          return;
        }

        if (payload.type === 'tool_called' && payload.conversation_id) {
          updateRunStatus(
            {
              active: true,
              phase: `Calling ${payload.tool ?? 'tool'}`,
              detail: String(payload.arguments_preview ?? 'Preparing tool input.'),
              conversationId: payload.conversation_id,
              runId: payload.run_id,
            },
            {
              label: `Calling ${payload.tool ?? 'tool'}`,
              detail: String(payload.arguments_preview ?? ''),
            },
          );
          return;
        }

        if (payload.type === 'tool_started' && payload.conversation_id) {
          updateRunStatus({
            active: true,
            phase: `Running ${payload.tool ?? 'tool'}`,
            detail: 'Waiting for this tool to finish.',
            conversationId: payload.conversation_id,
            runId: payload.run_id,
          });
          return;
        }

        if (payload.type === 'tool_finished' && payload.conversation_id) {
            updateRunStatus(
              {
                active: true,
                phase: `${payload.tool ?? 'Tool'} finished`,
                detail: String(payload.result_preview ?? ''),
                conversationId: payload.conversation_id,
                runId: payload.run_id,
              },
              {
                label: `${payload.tool ?? 'tool'} finished`,
                detail: String(payload.result_preview ?? ''),
                tone: payload.is_error ? 'warn' : 'success',
              },
            );
            addActivity({
                conversationId: payload.conversation_id,
                label: `${payload.tool ?? 'tool'} finished`,
                detail: String(payload.result_preview ?? ''),
                tone: payload.is_error ? 'warn' : 'success',
            });
            // When tool finishes, it will be added to DB and sent as message_created event
            return;
        }

        if (payload.type === 'reflection_retry' && payload.conversation_id) {
          updateRunStatus(
            {
              active: true,
              phase: 'Checking answer quality',
              detail: String(payload.feedback ?? 'The draft needs another pass.'),
              conversationId: payload.conversation_id,
              runId: payload.run_id,
            },
            { label: 'Quality retry', detail: String(payload.feedback ?? ''), tone: 'warn' },
          );
          return;
        }

        if (payload.type === 'message_created' && payload.conversation_id && payload.message) {
            const incoming = toChatMessage(payload.message);
            if (payload.conversation_id === activeConversationRef.current) {
                setMessages((prev) => {
                    const existingIndex = prev.findIndex((item) => item.id === incoming.id);
                    if (existingIndex !== -1) {
                        const next = [...prev];
                        next[existingIndex] = { ...next[existingIndex], ...incoming, pending: false };
                        return next;
                    }

                    if (
                        incoming.role === 'assistant' &&
                        currentRunRef.current?.pendingAssistantId &&
                        prev.some((item) => item.id === currentRunRef.current!.pendingAssistantId)
                    ) {
                        return prev.map((item) =>
                            item.id === currentRunRef.current!.pendingAssistantId ? { ...incoming, pending: false } : item,
                        );
                    }
                    
                    return [...prev, incoming];
                });

                if (incoming.role === 'assistant' && currentRunRef.current?.pendingAssistantId) {
                    currentRunRef.current = { ...currentRunRef.current, pendingAssistantId: null };
                }
            }
            return;
        }

        if (payload.type === 'run_completed') {
          setIsSending(false);
          currentRunRef.current = null;
          updateRunStatus(
            {
              active: false,
              phase: 'Run complete',
              detail: 'The final reply is saved in this thread.',
              conversationId: payload.conversation_id,
              runId: payload.run_id,
            },
            { label: 'Run complete', detail: 'Final reply saved.', tone: 'success' },
          );
          refreshConversations();
          return;
        }

        if (payload.type === 'run_cancelled' || payload.type === 'error') {
            setIsSending(false);
            if (payload.type === 'error') setStatusMessage(payload.error ?? 'Streaming error');
            updateRunStatus(
              {
                active: false,
                phase: payload.type === 'run_cancelled' ? 'Run cancelled' : 'Run failed',
                detail:
                  payload.type === 'run_cancelled'
                    ? 'The active run was cancelled.'
                    : payload.error ?? 'Streaming error',
                conversationId: payload.conversation_id,
                runId: payload.run_id,
              },
              {
                label: payload.type === 'run_cancelled' ? 'Run cancelled' : 'Run failed',
                detail:
                  payload.type === 'run_cancelled'
                    ? 'Cancellation was acknowledged.'
                    : payload.error ?? 'Streaming error',
                tone: 'warn',
              },
            );
            const pendingId = currentRunRef.current?.pendingAssistantId;
            if (pendingId) {
                setMessages((prev) =>
                    prev.map((item) => item.id === pendingId ? { ...item, pending: false } : item),
                );
            }
            currentRunRef.current = null;
            ws.close();
        }
      } catch (error) {
        console.error('Failed to parse streaming event:', error);
      }
    };

    ws.onerror = () => {
      setStatusMessage('WebSocket error. Check the server and token.');
      updateRunStatus(
        { active: false, phase: 'Connection failed', detail: 'The WebSocket connection failed.' },
        { label: 'Connection failed', detail: 'Check the server and token.', tone: 'warn' },
      );
      ws.close();
    };

    ws.onclose = () => {
      setIsSending(false);
    };
  }, [apiConfig, canConnect, isSending, pendingFileIds, isStreaming, activeConversationId, setStatusMessage, setActiveConversationId, refreshConversations, addActivity, ensurePendingAssistant, updateRunStatus]);

  const cancelRun = useCallback(async () => {
    if (!activeConversationId || !canConnect || !isSending) return;
    try {
      await cancelConversation(apiConfig, activeConversationId);
      setStatusMessage('Cancellation requested.');
    } catch (error) {
      setStatusMessage(error instanceof Error ? error.message : String(error));
    }
  }, [apiConfig, canConnect, isSending, activeConversationId, setStatusMessage]);

  return {
    messages,
    isSending,
    isStreaming,
    setIsStreaming,
    runStatus,
    uploading,
    pendingFileIds,
    uploadFile,
    sendMessage,
    cancelRun,
    refreshMessages,
  };
};
