import { useEffect, useMemo, useRef, useState } from 'react'
import type { FormEvent, KeyboardEvent } from 'react'
import ReactMarkdown from 'react-markdown'
import {
  addAccount,
  buildWebSocketUrl,
  cancelConversation,
  deleteAccount,
  getMessages,
  listAccounts,
  listConversations,
  listOnboarding,
  sendMessage,
} from './api'
import type { ApiConfig } from './api'
import { KnowledgeGraph } from './components/KnowledgeGraph'
import type {
  Account,
  Conversation,
  Message,
  OnboardingStatus,
  ToolCall,
} from './types'

type ChatMessage = {
  id: string
  role: 'user' | 'assistant' | 'tool' | 'system'
  content: string
  created_at?: string
  pending?: boolean
  name?: string | null
  toolCallId?: string | null
  toolCalls?: ToolCall[]
}

type ActivityItem = {
  id: string
  conversationId?: string
  label: string
  detail: string
  at: string
  tone?: 'normal' | 'success' | 'warn'
}

type CurrentRun = {
  conversationId?: string
  runId?: string
  pendingAssistantId?: string | null
}

type ServerEvent = {
  type: string
  conversation_id?: string
  run_id?: string
  iteration?: number
  content?: string
  reason?: string
  tool?: string
  call_id?: string
  arguments_preview?: string
  result_preview?: string
  is_error?: boolean
  feedback?: string
  title?: string | null
  updated_at?: string
  source?: string
  message?: Message
  error?: string
}

const INITIAL_MESSAGE_LIMIT = 100

const getDefaultBaseUrl = () => {
  if (typeof window === 'undefined') {
    return ''
  }

  const envBase = import.meta.env.VITE_API_BASE as string | undefined
  if (envBase) {
    return envBase
  }

  const { hostname, port, protocol } = window.location
  const isLocalHost = hostname === 'localhost' || hostname === '127.0.0.1'
  const isVitePort = port === '5173' || port === '5174' || port === '4173'

  if (isLocalHost && isVitePort) {
    return `${protocol}//${hostname}:3000`
  }

  return window.location.origin
}

const DEFAULT_CONFIG: ApiConfig = {
  baseUrl: getDefaultBaseUrl(),
  token: '',
}

const loadConfig = (): ApiConfig => {
  if (typeof window === 'undefined') {
    return DEFAULT_CONFIG
  }

  const stored = window.localStorage.getItem('jossie_api')
  if (!stored) {
    return DEFAULT_CONFIG
  }

  try {
    const parsed = JSON.parse(stored) as ApiConfig
    return { ...DEFAULT_CONFIG, ...parsed }
  } catch {
    return DEFAULT_CONFIG
  }
}

const persistConfig = (config: ApiConfig) => {
  if (typeof window === 'undefined') {
    return
  }

  window.localStorage.setItem('jossie_api', JSON.stringify(config))
}

const createLocalId = () => `local-${Math.random().toString(36).slice(2, 10)}`

const formatError = (error: unknown) => {
  if (error instanceof Error) {
    return error.message
  }
  return 'Something went wrong. Check the server and token.'
}

const formatRelativeTime = (value?: string) => {
  if (!value) {
    return ''
  }

  const date = new Date(value)
  if (Number.isNaN(date.getTime())) {
    return value
  }

  const diffMs = date.getTime() - Date.now()
  const diffMinutes = Math.round(diffMs / 60000)
  const formatter = new Intl.RelativeTimeFormat(undefined, { numeric: 'auto' })

  if (Math.abs(diffMinutes) < 60) {
    return formatter.format(diffMinutes, 'minute')
  }

  const diffHours = Math.round(diffMinutes / 60)
  if (Math.abs(diffHours) < 48) {
    return formatter.format(diffHours, 'hour')
  }

  const diffDays = Math.round(diffHours / 24)
  return formatter.format(diffDays, 'day')
}

const formatClockTime = (value?: string) => {
  if (!value) {
    return ''
  }

  const date = new Date(value)
  if (Number.isNaN(date.getTime())) {
    return value
  }

  return new Intl.DateTimeFormat(undefined, {
    hour: '2-digit',
    minute: '2-digit',
  }).format(date)
}

const prettifyContent = (content: string) => {
  try {
    const parsed = JSON.parse(content)
    return JSON.stringify(parsed, null, 2)
  } catch {
    return content
  }
}

const toChatMessage = (message: Message): ChatMessage => ({
  id: message.id,
  role: message.role,
  content: message.content,
  created_at: message.created_at,
  name: message.name ?? null,
  toolCallId: message.tool_call_id ?? null,
  toolCalls: message.tool_calls ?? undefined,
})

const toChatMessages = (messages: Message[]) => messages.map(toChatMessage)

const buildActivityFromMessages = (messages: ChatMessage[]): ActivityItem[] =>
  messages.flatMap((message) => {
    const at = formatClockTime(message.created_at)

    if (message.role === 'assistant' && message.toolCalls?.length) {
      return message.toolCalls.map((toolCall) => ({
        id: `${message.id}-${toolCall.id}`,
        label: `Planned ${toolCall.name}`,
        detail: toolCall.arguments,
        at,
      }))
    }

    if (message.role === 'tool') {
      return [
        {
          id: message.id,
          label: message.name ? `${message.name} finished` : 'Tool finished',
          detail: prettifyContent(message.content),
          at,
        },
      ]
    }

    return []
  })

const mergeServerMessage = (
  prev: ChatMessage[],
  incoming: ChatMessage,
  currentRun: CurrentRun | null,
) => {
  const existingIndex = prev.findIndex((item) => item.id === incoming.id)
  if (existingIndex !== -1) {
    const next = [...prev]
    next[existingIndex] = { ...next[existingIndex], ...incoming, pending: false }
    return next
  }

  if (
    incoming.role === 'assistant' &&
    currentRun?.pendingAssistantId &&
    prev.some((item) => item.id === currentRun.pendingAssistantId)
  ) {
    return prev.map((item) =>
      item.id === currentRun.pendingAssistantId ? { ...incoming, pending: false } : item,
    )
  }

  if (incoming.role === 'user') {
    const localIndex = prev.findIndex(
      (item) =>
        item.id.startsWith('local-') &&
        item.role === 'user' &&
        item.content.trim() === incoming.content.trim(),
    )
    if (localIndex !== -1) {
      const next = [...prev]
      next[localIndex] = incoming
      return next
    }
  }

  return [...prev, incoming]
}

const App = () => {
  const [apiConfig, setApiConfig] = useState<ApiConfig>(loadConfig)
  const [activeTab, setActiveTab] = useState<
    'assistant' | 'integrations' | 'accounts' | 'knowledge'
  >('assistant')
  const [conversations, setConversations] = useState<Conversation[]>([])
  const [activeConversationId, setActiveConversationId] = useState<string | null>(null)
  const [messages, setMessages] = useState<ChatMessage[]>([])
  const [composer, setComposer] = useState('')
  const [isStreaming, setIsStreaming] = useState(true)
  const [isSending, setIsSending] = useState(false)
  const [statusMessage, setStatusMessage] = useState<string | null>(null)
  const [onboarding, setOnboarding] = useState<OnboardingStatus[]>([])
  const [accounts, setAccounts] = useState<Account[]>([])
  const [activity, setActivity] = useState<ActivityItem[]>([])
  const [accountForm, setAccountForm] = useState({
    integration: 'email',
    name: '',
    username: '',
    password: '',
    imap_host: '',
    imap_port: '993',
    smtp_host: '',
    smtp_port: '587',
    refresh_token: '',
    google_email: '',
    customJson: '{\n  "key": "value"\n}',
  })
  const chatEndRef = useRef<HTMLDivElement>(null)
  const currentRunRef = useRef<CurrentRun | null>(null)
  const activeConversationRef = useRef<string | null>(null)

  useEffect(() => {
    persistConfig(apiConfig)
  }, [apiConfig])

  useEffect(() => {
    activeConversationRef.current = activeConversationId
  }, [activeConversationId])

  useEffect(() => {
    chatEndRef.current?.scrollIntoView({ behavior: 'smooth' })
  }, [messages, activity])

  const canConnect = useMemo(() => Boolean(apiConfig.baseUrl && apiConfig.token), [apiConfig])

  const addActivity = (item: Omit<ActivityItem, 'id' | 'at'>) => {
    setActivity((prev) => [
      {
        id: createLocalId(),
        at: new Date().toLocaleTimeString(),
        ...item,
      },
      ...prev,
    ].slice(0, 80))
  }

  const refreshConversations = async () => {
    if (!canConnect) {
      return
    }
    try {
      const data = await listConversations(apiConfig)
      setConversations(data)
    } catch (error) {
      setStatusMessage(formatError(error))
    }
  }

  const refreshMessages = async (conversationId: string) => {
    if (!canConnect) {
      return
    }
    try {
      const data = await getMessages(apiConfig, conversationId, INITIAL_MESSAGE_LIMIT)
      const chatMessages = toChatMessages(data)
      setMessages(chatMessages)
      setActivity(buildActivityFromMessages(chatMessages))
    } catch (error) {
      setStatusMessage(formatError(error))
    }
  }

  const refreshOnboarding = async () => {
    if (!canConnect) {
      return
    }
    try {
      const data = await listOnboarding(apiConfig)
      setOnboarding(data)
    } catch (error) {
      setStatusMessage(formatError(error))
    }
  }

  const refreshAccounts = async () => {
    if (!canConnect) {
      return
    }
    try {
      const data = await listAccounts(apiConfig)
      setAccounts(data)
    } catch (error) {
      setStatusMessage(formatError(error))
    }
  }

  useEffect(() => {
    refreshConversations()
  }, [apiConfig.baseUrl, apiConfig.token])

  useEffect(() => {
    if (activeTab === 'integrations') {
      refreshOnboarding()
    }
    if (activeTab === 'accounts') {
      refreshAccounts()
    }
  }, [activeTab, apiConfig.baseUrl, apiConfig.token])

  useEffect(() => {
    if (!canConnect) {
      return
    }

    const ws = new WebSocket(buildWebSocketUrl(apiConfig, '/api/events'))

    ws.onmessage = (event) => {
      try {
        const payload = JSON.parse(event.data) as ServerEvent
        if (payload.type === 'conversation_updated' && payload.conversation_id) {
          setConversations((prev) => {
            const existing = prev.find(
              (conversation) => conversation.id === payload.conversation_id,
            )
            const nextConversation: Conversation = existing
              ? {
                  ...existing,
                  title: payload.title ?? existing.title,
                  updated_at: payload.updated_at ?? existing.updated_at,
                }
              : {
                  id: payload.conversation_id,
                  title: payload.title ?? null,
                  created_at: payload.updated_at ?? new Date().toISOString(),
                  updated_at: payload.updated_at ?? new Date().toISOString(),
                }

            const next = [
              nextConversation,
              ...prev.filter((conversation) => conversation.id !== payload.conversation_id),
            ]
            next.sort(
              (left, right) =>
                new Date(right.updated_at).getTime() - new Date(left.updated_at).getTime(),
            )
            return next
          })
          return
        }

        if (payload.type === 'message_created' && payload.conversation_id && payload.message) {
          const incoming = toChatMessage(payload.message)
          const selectedConversationId =
            activeConversationRef.current ?? currentRunRef.current?.conversationId ?? null

          if (selectedConversationId === payload.conversation_id) {
            setMessages((prev) =>
              mergeServerMessage(prev, incoming, currentRunRef.current),
            )
            if (incoming.role === 'assistant' && currentRunRef.current?.pendingAssistantId) {
              currentRunRef.current = {
                ...currentRunRef.current,
                pendingAssistantId: null,
              }
            }
            if (incoming.role === 'tool') {
              addActivity({
                conversationId: payload.conversation_id,
                label: incoming.name ? `${incoming.name} finished` : 'Tool finished',
                detail: prettifyContent(incoming.content),
                tone: 'success',
              })
            }
          }
          return
        }

        if (payload.type === 'background_notification' && payload.conversation_id) {
          const detail =
            typeof payload.message === 'string'
              ? payload.message
              : payload.content ?? ''
          addActivity({
            conversationId: payload.conversation_id,
            label: payload.source ? `${payload.source} update` : 'Background update',
            detail,
            tone: 'success',
          })
          return
        }

        if (payload.type === 'cancel_requested' && payload.conversation_id) {
          addActivity({
            conversationId: payload.conversation_id,
            label: 'Cancel requested',
            detail: 'The current run will stop at the next safe checkpoint.',
            tone: 'warn',
          })
        }
      } catch (error) {
        setStatusMessage(formatError(error))
      }
    }

    ws.onerror = () => {
      setStatusMessage('Background events connection failed.')
    }

    return () => {
      ws.close()
    }
  }, [apiConfig.baseUrl, apiConfig.token, canConnect])

  const handleSelectConversation = (conversation: Conversation) => {
    setActiveConversationId(conversation.id)
    refreshMessages(conversation.id)
  }

  const handleStartNewConversation = () => {
    setActiveConversationId(null)
    currentRunRef.current = null
    setMessages([])
    setActivity([])
  }

  const ensurePendingAssistant = (conversationId?: string) => {
    if (currentRunRef.current?.pendingAssistantId) {
      return currentRunRef.current.pendingAssistantId
    }

    const assistantId = createLocalId()
    currentRunRef.current = {
      ...currentRunRef.current,
      conversationId,
      pendingAssistantId: assistantId,
    }
    setMessages((prev) => [
      ...prev,
      { id: assistantId, role: 'assistant', content: '', pending: true },
    ])
    return assistantId
  }

  const handleSend = async () => {
    const trimmed = composer.trim()
    if (!trimmed || !canConnect || isSending) {
      return
    }

    setStatusMessage(null)
    setIsSending(true)

    const userMessage: ChatMessage = {
      id: createLocalId(),
      role: 'user',
      content: trimmed,
    }

    setComposer('')
    setMessages((prev) => [...prev, userMessage])

    if (!isStreaming) {
      try {
        const result = await sendMessage(apiConfig, trimmed, activeConversationId)
        const assistantMessage: ChatMessage = {
          id: createLocalId(),
          role: 'assistant',
          content: result.message,
        }
        setMessages((prev) => [...prev, assistantMessage])
        setActiveConversationId(result.conversation_id)
        refreshConversations()
      } catch (error) {
        setStatusMessage(formatError(error))
      } finally {
        setIsSending(false)
      }
      return
    }

    const localAssistantId = ensurePendingAssistant(activeConversationId ?? undefined)
    currentRunRef.current = {
      conversationId: activeConversationId ?? undefined,
      pendingAssistantId: localAssistantId,
    }

    const ws = new WebSocket(buildWebSocketUrl(apiConfig, '/api/chat/stream'))

    ws.onopen = () => {
      ws.send(
        JSON.stringify({
          message: trimmed,
          ...(activeConversationId ? { conversation_id: activeConversationId } : {}),
        }),
      )
    }

    ws.onmessage = (event) => {
      try {
        const payload = JSON.parse(event.data) as ServerEvent

        if (payload.type === 'run_started' && payload.conversation_id) {
          currentRunRef.current = {
            ...currentRunRef.current,
            runId: payload.run_id,
            conversationId: payload.conversation_id,
          }
          setActiveConversationId(payload.conversation_id)
          addActivity({
            conversationId: payload.conversation_id,
            label: 'Run started',
            detail: 'The assistant has started working on this request.',
          })
          return
        }

        if (payload.type === 'assistant_thinking' && payload.conversation_id) {
          ensurePendingAssistant(payload.conversation_id)
          addActivity({
            conversationId: payload.conversation_id,
            label: `Thinking pass ${Number(payload.iteration ?? 0) + 1}`,
            detail: 'Planning the next step.',
          })
          return
        }

        if (payload.type === 'assistant_delta') {
          const pendingId = ensurePendingAssistant(payload.conversation_id)
          const delta = String(payload.content ?? '')
          setMessages((prev) =>
            prev.map((item) =>
              item.id === pendingId ? { ...item, content: `${item.content}${delta}` } : item,
            ),
          )
          return
        }

        if (payload.type === 'assistant_reset') {
          const pendingId = currentRunRef.current?.pendingAssistantId
          if (pendingId) {
            setMessages((prev) =>
              prev.map((item) =>
                item.id === pendingId ? { ...item, content: '', pending: true } : item,
              ),
            )
          }
          return
        }

        if (payload.type === 'tool_called' && payload.conversation_id) {
          addActivity({
            conversationId: payload.conversation_id,
            label: `Queued ${payload.tool ?? 'tool'}`,
            detail: String(payload.arguments_preview ?? ''),
          })
          return
        }

        if (payload.type === 'tool_started' && payload.conversation_id) {
          addActivity({
            conversationId: payload.conversation_id,
            label: `Running ${payload.tool ?? 'tool'}`,
            detail: 'Waiting for a result.',
          })
          return
        }

        if (payload.type === 'tool_finished' && payload.conversation_id) {
          addActivity({
            conversationId: payload.conversation_id,
            label: `${payload.tool ?? 'tool'} finished`,
            detail: String(payload.result_preview ?? ''),
            tone: payload.is_error ? 'warn' : 'success',
          })
          return
        }

        if (payload.type === 'reflection_retry' && payload.conversation_id) {
          addActivity({
            conversationId: payload.conversation_id,
            label: 'Reflection retry',
            detail: String(payload.feedback ?? ''),
            tone: 'warn',
          })
          return
        }

        if (payload.type === 'run_completed') {
          setIsSending(false)
          refreshConversations()
          return
        }

        if (payload.type === 'run_cancelled') {
          setIsSending(false)
          setStatusMessage('Run cancelled.')
          const pendingId = currentRunRef.current?.pendingAssistantId
          if (pendingId) {
            setMessages((prev) =>
              prev.map((item) =>
                item.id === pendingId ? { ...item, pending: false } : item,
              ),
            )
          }
          currentRunRef.current = null
          ws.close()
          return
        }

        if (payload.type === 'error') {
          setStatusMessage(String(payload.error ?? 'Streaming error'))
          setIsSending(false)
          const pendingId = currentRunRef.current?.pendingAssistantId
          if (pendingId) {
            setMessages((prev) =>
              prev.map((item) =>
                item.id === pendingId ? { ...item, pending: false } : item,
              ),
            )
          }
          currentRunRef.current = null
          ws.close()
        }
      } catch (error) {
        setStatusMessage(formatError(error))
      }
    }

    ws.onerror = () => {
      setStatusMessage('WebSocket error. Check the server and token.')
      ws.close()
    }

    ws.onclose = () => {
      setIsSending(false)
    }
  }

  const handleCancelRun = async () => {
    const conversationId =
      currentRunRef.current?.conversationId ?? activeConversationId ?? null
    if (!conversationId || !canConnect || !isSending) {
      return
    }

    try {
      await cancelConversation(apiConfig, conversationId)
      setStatusMessage('Cancellation requested.')
    } catch (error) {
      setStatusMessage(formatError(error))
    }
  }

  const handleComposerKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key === 'Enter' && !event.shiftKey) {
      event.preventDefault()
      handleSend()
    }
  }

  const handleAccountSubmit = async (event: FormEvent) => {
    event.preventDefault()
    if (!canConnect) {
      return
    }

    try {
      if (accountForm.integration === 'google' && !accountForm.refresh_token.trim()) {
        setStatusMessage('Google refresh token is required for manual setup.')
        return
      }

      let payload: { integration: string; name: string; config: Record<string, unknown> }

      if (accountForm.integration === 'email') {
        payload = {
          integration: 'email',
          name: accountForm.name || 'Email Account',
          config: {
            username: accountForm.username,
            password: accountForm.password,
            imap_host: accountForm.imap_host,
            imap_port: Number(accountForm.imap_port),
            smtp_host: accountForm.smtp_host,
            smtp_port: Number(accountForm.smtp_port),
          },
        }
      } else if (accountForm.integration === 'google') {
        payload = {
          integration: 'google',
          name: accountForm.name || 'Google Account',
          config: {
            refresh_token: accountForm.refresh_token,
            email: accountForm.google_email,
          },
        }
      } else {
        payload = {
          integration: accountForm.integration,
          name: accountForm.name || 'Custom Account',
          config: JSON.parse(accountForm.customJson || '{}') as Record<string, unknown>,
        }
      }

      await addAccount(apiConfig, payload)
      setStatusMessage('Account added.')
      refreshAccounts()
    } catch (error) {
      setStatusMessage(formatError(error))
    }
  }

  const handleDeleteAccount = async (accountId: string) => {
    if (!canConnect) {
      return
    }
    try {
      await deleteAccount(apiConfig, accountId)
      refreshAccounts()
    } catch (error) {
      setStatusMessage(formatError(error))
    }
  }

  const googleOauthUrl = useMemo(() => {
    const base = apiConfig.baseUrl.replace(/\/+$/, '')
    if (!apiConfig.token) {
      return `${base}/setup/google`
    }
    const encoded = encodeURIComponent(apiConfig.token)
    return `${base}/setup/google?token=${encoded}`
  }, [apiConfig.baseUrl, apiConfig.token])

  const buildGoogleOauthUrl = (accountName?: string) => {
    try {
      const url = new URL(googleOauthUrl)
      const trimmed = accountName?.trim()
      if (trimmed) {
        url.searchParams.set('account_name', trimmed)
      }
      return url.toString()
    } catch {
      return googleOauthUrl
    }
  }

  const handleGoogleConnect = (accountName?: string) => {
    window.open(buildGoogleOauthUrl(accountName), '_blank')
  }

  const visibleActivity = useMemo(() => {
    if (!activeConversationId) {
      return activity
    }
    return activity.filter(
      (item) => !item.conversationId || item.conversationId === activeConversationId,
    )
  }, [activity, activeConversationId])

  const suggestionPrompts = useMemo(() => {
    if (messages.length === 0) {
      return [
        'Summarize my current setup and what you can do for me.',
        'Check for anything new that needs my attention.',
        'Help me create a recurring review workflow.',
      ]
    }

    const lastAssistant = [...messages]
      .reverse()
      .find((message) => message.role === 'assistant')

    if (!lastAssistant) {
      return [
        'Summarize the latest activity.',
        'What should I do next?',
        'Turn this into a scheduled follow-up.',
      ]
    }

    return [
      'Summarize this thread in three bullets.',
      'List any follow-ups or reminders I should create.',
      'Save the important facts from this thread to memory.',
    ]
  }, [messages])

  return (
    <div className="app">
      <aside className="sidebar">
        <div className="brand">
          <div className="brand-mark">J</div>
          <div>
            <p className="brand-title">Jossie</p>
            <p className="brand-subtitle">Control Deck</p>
          </div>
        </div>

        <div className="tabs">
          <button
            className={activeTab === 'assistant' ? 'tab active' : 'tab'}
            onClick={() => setActiveTab('assistant')}
          >
            Assistant
          </button>
          <button
            className={activeTab === 'integrations' ? 'tab active' : 'tab'}
            onClick={() => setActiveTab('integrations')}
          >
            Integrations
          </button>
          <button
            className={activeTab === 'accounts' ? 'tab active' : 'tab'}
            onClick={() => setActiveTab('accounts')}
          >
            Accounts
          </button>
          <button
            className={activeTab === 'knowledge' ? 'tab active' : 'tab'}
            onClick={() => setActiveTab('knowledge')}
          >
            Knowledge
          </button>
        </div>

        {activeTab === 'assistant' && (
          <div className="conversation-panel">
            <div className="panel-header">
              <h3>Conversations</h3>
              <button className="button ghost" onClick={handleStartNewConversation}>
                New
              </button>
            </div>
            <div className="conversation-list">
              {conversations.length === 0 && (
                <p className="empty">No conversations yet.</p>
              )}
              {conversations.map((conversation, index) => (
                <button
                  key={conversation.id}
                  className={
                    conversation.id === activeConversationId
                      ? 'conversation active'
                      : 'conversation'
                  }
                  style={{ ['--delay' as string]: `${index * 40}ms` }}
                  onClick={() => handleSelectConversation(conversation)}
                >
                  <span className="conversation-title">
                    {conversation.title ?? 'Untitled conversation'}
                  </span>
                  <span className="conversation-meta">
                    {formatRelativeTime(conversation.updated_at)}
                  </span>
                </button>
              ))}
            </div>
          </div>
        )}

        <div className="status-card">
          <p className="status-title">Connection</p>
          <p className={canConnect ? 'status-ok' : 'status-warn'}>
            {canConnect ? 'Ready' : 'Token required'}
          </p>
          {statusMessage && <p className="status-message">{statusMessage}</p>}
        </div>
      </aside>

      <main className="main">
        {activeTab === 'assistant' && (
          <section className="assistant">
            <header className="section-header">
              <div>
                <h1>Command the assistant</h1>
                <p>Live runs, inline tool cards, and browser-visible background activity.</p>
              </div>
              <div className="chip-row">
                <span className={isStreaming ? 'chip active' : 'chip'}>
                  {isStreaming ? 'Streaming on' : 'Streaming off'}
                </span>
                <span className={isSending ? 'chip active' : 'chip'}>
                  {isSending ? 'Run active' : 'Idle'}
                </span>
                <span className="chip">API {apiConfig.baseUrl}</span>
              </div>
            </header>

            <div className="chat-shell">
              <div className="chat-feed">
                {messages.length === 0 && (
                  <div className="empty hero">
                    <h2>Start a new briefing</h2>
                    <p>
                      Ask the assistant to orchestrate emails, calendars, schedules,
                      and background follow-ups.
                    </p>
                  </div>
                )}
                {messages.map((message) => (
                  <div
                    key={message.id}
                    className={`message ${message.role} ${message.pending ? 'pending' : ''}`}
                  >
                    <div className="message-header">
                      <div className="message-role">
                        {message.name ? `${message.role} · ${message.name}` : message.role}
                      </div>
                      {message.created_at && (
                        <div className="message-meta">
                          {formatClockTime(message.created_at)}
                        </div>
                      )}
                    </div>
                    <div className="message-content markdown">
                      <ReactMarkdown>{prettifyContent(message.content)}</ReactMarkdown>
                    </div>
                    {message.toolCalls?.length ? (
                      <div className="tool-call-list">
                        {message.toolCalls.map((toolCall) => (
                          <div key={toolCall.id} className="tool-call">
                            <p className="tool-call-name">{toolCall.name}</p>
                            <pre className="tool-call-args">{toolCall.arguments}</pre>
                          </div>
                        ))}
                      </div>
                    ) : null}
                  </div>
                ))}
                <div ref={chatEndRef} />
              </div>

              <div className="composer">
                <textarea
                  placeholder="Ask Jossie to coordinate your day..."
                  value={composer}
                  onChange={(event) => setComposer(event.target.value)}
                  onKeyDown={handleComposerKeyDown}
                />
                <div className="quick-actions">
                  {suggestionPrompts.map((prompt) => (
                    <button
                      key={prompt}
                      className="chip action-chip"
                      onClick={() => setComposer(prompt)}
                      type="button"
                    >
                      {prompt}
                    </button>
                  ))}
                </div>
                <div className="composer-actions">
                  <label className="toggle">
                    <input
                      type="checkbox"
                      checked={isStreaming}
                      onChange={(event) => setIsStreaming(event.target.checked)}
                    />
                    <span>Stream</span>
                  </label>
                  <div className="composer-button-row">
                    <button
                      className="button ghost"
                      onClick={handleCancelRun}
                      disabled={!isSending}
                    >
                      Cancel
                    </button>
                    <button
                      className="button primary"
                      onClick={handleSend}
                      disabled={!canConnect || isSending}
                    >
                      {isSending ? 'Running...' : 'Send'}
                    </button>
                  </div>
                </div>
              </div>
            </div>
          </section>
        )}

        {activeTab === 'integrations' && (
          <section className="integrations">
            <header className="section-header">
              <div>
                <h1>Integrations control</h1>
                <p>Track onboarding status and launch new connections.</p>
              </div>
              <div className="chip-row">
                <button className="button primary" onClick={() => handleGoogleConnect()}>
                  Login with Google
                </button>
                <button className="button ghost" onClick={refreshOnboarding}>
                  Refresh
                </button>
              </div>
            </header>

            <div className="grid">
              {onboarding.map((integration, index) => (
                <div
                  key={integration.name}
                  className="card"
                  style={{ ['--delay' as string]: `${index * 60}ms` }}
                >
                  <div className="card-header">
                    <div>
                      <h3>{integration.name}</h3>
                      <p className="muted">Status: {integration.status}</p>
                    </div>
                    {integration.name === 'google' && (
                      <button
                        className="button primary"
                        onClick={() => handleGoogleConnect()}
                      >
                        Connect
                      </button>
                    )}
                  </div>
                  {integration.details?.fields && (
                    <div className="field-list">
                      {integration.details.fields.map((field) => (
                        <div key={field.name} className="field">
                          <span>{field.label ?? field.name}</span>
                          <span className="muted">{field.type ?? 'text'}</span>
                        </div>
                      ))}
                    </div>
                  )}
                </div>
              ))}
            </div>

            <div className="card form-card">
              <div className="card-header">
                <div>
                  <h3>Add account</h3>
                  <p className="muted">Use email or custom JSON config.</p>
                </div>
              </div>
              <form className="form" onSubmit={handleAccountSubmit}>
                <label>
                  Integration
                  <select
                    value={accountForm.integration}
                    onChange={(event) =>
                      setAccountForm((prev) => ({
                        ...prev,
                        integration: event.target.value,
                      }))
                    }
                  >
                    <option value="email">email</option>
                    <option value="google">google (oauth)</option>
                    <option value="custom">custom</option>
                  </select>
                </label>
                <label>
                  Friendly name
                  <input
                    value={accountForm.name}
                    onChange={(event) =>
                      setAccountForm((prev) => ({
                        ...prev,
                        name: event.target.value,
                      }))
                    }
                    placeholder="Work inbox"
                  />
                </label>

                {accountForm.integration === 'email' && (
                  <div className="field-grid">
                    <label>
                      Username
                      <input
                        value={accountForm.username}
                        onChange={(event) =>
                          setAccountForm((prev) => ({
                            ...prev,
                            username: event.target.value,
                          }))
                        }
                        placeholder="me@example.com"
                      />
                    </label>
                    <label>
                      Password
                      <input
                        type="password"
                        value={accountForm.password}
                        onChange={(event) =>
                          setAccountForm((prev) => ({
                            ...prev,
                            password: event.target.value,
                          }))
                        }
                        placeholder="app password"
                      />
                    </label>
                    <label>
                      IMAP host
                      <input
                        value={accountForm.imap_host}
                        onChange={(event) =>
                          setAccountForm((prev) => ({
                            ...prev,
                            imap_host: event.target.value,
                          }))
                        }
                        placeholder="imap.example.com"
                      />
                    </label>
                    <label>
                      IMAP port
                      <input
                        value={accountForm.imap_port}
                        onChange={(event) =>
                          setAccountForm((prev) => ({
                            ...prev,
                            imap_port: event.target.value,
                          }))
                        }
                      />
                    </label>
                    <label>
                      SMTP host
                      <input
                        value={accountForm.smtp_host}
                        onChange={(event) =>
                          setAccountForm((prev) => ({
                            ...prev,
                            smtp_host: event.target.value,
                          }))
                        }
                        placeholder="smtp.example.com"
                      />
                    </label>
                    <label>
                      SMTP port
                      <input
                        value={accountForm.smtp_port}
                        onChange={(event) =>
                          setAccountForm((prev) => ({
                            ...prev,
                            smtp_port: event.target.value,
                          }))
                        }
                      />
                    </label>
                  </div>
                )}

                {accountForm.integration === 'google' && (
                  <div className="callout">
                    <p>
                      Use OAuth to connect Google. Click Connect in the integration
                      card or open {googleOauthUrl}. The OAuth flow creates a
                      Google account entry. Add a friendly name and click Login
                      with Google to label the account.
                    </p>
                    <button
                      className="button primary"
                      type="button"
                      onClick={() => handleGoogleConnect(accountForm.name)}
                    >
                      Login with Google
                    </button>
                  </div>
                )}

                {accountForm.integration === 'google' && (
                  <div className="field-grid">
                    <label>
                      Refresh token
                      <input
                        value={accountForm.refresh_token}
                        onChange={(event) =>
                          setAccountForm((prev) => ({
                            ...prev,
                            refresh_token: event.target.value,
                          }))
                        }
                        placeholder="Paste Google refresh token"
                      />
                    </label>
                    <label>
                      Account email (optional)
                      <input
                        value={accountForm.google_email}
                        onChange={(event) =>
                          setAccountForm((prev) => ({
                            ...prev,
                            google_email: event.target.value,
                          }))
                        }
                        placeholder="name@gmail.com"
                      />
                    </label>
                  </div>
                )}

                {accountForm.integration === 'custom' && (
                  <label>
                    Config JSON
                    <textarea
                      rows={6}
                      value={accountForm.customJson}
                      onChange={(event) =>
                        setAccountForm((prev) => ({
                          ...prev,
                          customJson: event.target.value,
                        }))
                      }
                    />
                  </label>
                )}

                <button className="button primary" type="submit" disabled={!canConnect}>
                  Add account
                </button>
              </form>
            </div>
          </section>
        )}

        {activeTab === 'accounts' && (
          <section className="accounts">
            <header className="section-header">
              <div>
                <h1>Accounts</h1>
                <p>Manage configured integrations stored by the server.</p>
              </div>
              <button className="button ghost" onClick={refreshAccounts}>
                Refresh
              </button>
            </header>

            <div className="grid">
              {accounts.map((account, index) => (
                <div
                  key={account.id}
                  className="card"
                  style={{ ['--delay' as string]: `${index * 60}ms` }}
                >
                  <div className="card-header">
                    <div>
                      <h3>{account.name}</h3>
                      <p className="muted">{account.integration}</p>
                    </div>
                    <button
                      className="button ghost"
                      onClick={() => handleDeleteAccount(account.id)}
                    >
                      Remove
                    </button>
                  </div>
                  <pre className="code">
                    {JSON.stringify(account.details ?? {}, null, 2)}
                  </pre>
                </div>
              ))}
            </div>
          </section>
        )}

        {activeTab === 'knowledge' && (
          <section
            className="knowledge"
            style={{ height: '100%', display: 'flex', flexDirection: 'column' }}
          >
            <header className="section-header">
              <div>
                <h1>Knowledge Graph</h1>
                <p>Visualize relationships and entities in the memory.</p>
              </div>
            </header>
            <div className="card" style={{ flex: 1, padding: 0, overflow: 'hidden' }}>
              <KnowledgeGraph apiConfig={apiConfig} />
            </div>
          </section>
        )}
      </main>

      <aside className="inspector">
        <div className="card">
          <h3>API settings</h3>
          <div className="form">
            <label>
              Base URL
              <input
                value={apiConfig.baseUrl}
                onChange={(event) =>
                  setApiConfig((prev) => ({ ...prev, baseUrl: event.target.value }))
                }
                placeholder="http://localhost:8080"
              />
            </label>
            <label>
              Token
              <input
                value={apiConfig.token}
                onChange={(event) =>
                  setApiConfig((prev) => ({ ...prev, token: event.target.value }))
                }
                placeholder="Paste auth token"
              />
            </label>
            <button className="button ghost" onClick={refreshConversations}>
              Test connection
            </button>
          </div>
        </div>

        <div className="card">
          <div className="card-header">
            <h3>Live activity</h3>
            <span className="muted">{visibleActivity.length} events</span>
          </div>
          <div className="activity">
            {visibleActivity.length === 0 && (
              <p className="empty">No activity for this conversation yet.</p>
            )}
            {visibleActivity.map((event) => (
              <div
                key={event.id}
                className={`activity-item ${event.tone ? `tone-${event.tone}` : ''}`}
              >
                <div>
                  <p className="activity-tool">{event.label}</p>
                  <p className="activity-time">{event.at}</p>
                </div>
                <p className="activity-result">{event.detail}</p>
              </div>
            ))}
          </div>
        </div>

        <div className="card highlight">
          <h3>Runtime model</h3>
          <p className="muted">
            The web UI now listens for background notifications, conversation updates,
            and structured run events instead of waiting for plain-text replies only.
          </p>
          <div className="chip-row">
            <span className="chip">Live Runs</span>
            <span className="chip">Schedules</span>
            <span className="chip">Background Events</span>
          </div>
        </div>
      </aside>
    </div>
  )
}

export default App
