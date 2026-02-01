import { useEffect, useMemo, useRef, useState } from 'react'
import type { FormEvent, KeyboardEvent } from 'react'
import ReactMarkdown from 'react-markdown'
import {
  addAccount,
  buildWebSocketUrl,
  deleteAccount,
  getMessages,
  listAccounts,
  listConversations,
  listOnboarding,
  sendMessage,
} from './api'
import type { ApiConfig } from './api'
import { KnowledgeGraph } from './components/KnowledgeGraph'
import type { Account, Conversation, Message, OnboardingStatus } from './types'

type ChatMessage = {
  id: string
  role: 'user' | 'assistant' | 'tool'
  content: string
  created_at?: string
  pending?: boolean
}

type ActivityItem = {
  id: string
  tool: string
  result: string
  at: string
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

const toChatMessages = (messages: Message[]): ChatMessage[] =>
  messages.map((message) => ({
    id: message.id,
    role:
      message.role === 'assistant'
        ? 'assistant'
        : message.role === 'tool'
          ? 'tool'
          : 'user',
    content: message.content,
    created_at: message.created_at,
  }))

const App = () => {
  const [apiConfig, setApiConfig] = useState<ApiConfig>(loadConfig)
  const [activeTab, setActiveTab] = useState<'assistant' | 'integrations' | 'accounts' | 'knowledge'>(
    'assistant',
  )
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
  const chatEndRef = useRef<HTMLDivElement>(null)
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

  useEffect(() => {
    persistConfig(apiConfig)
  }, [apiConfig])

  useEffect(() => {
    chatEndRef.current?.scrollIntoView({ behavior: 'smooth' })
  }, [messages])

  const canConnect = useMemo(() => Boolean(apiConfig.baseUrl && apiConfig.token), [apiConfig])

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
      setMessages(toChatMessages(data))
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

  const handleSelectConversation = (conversation: Conversation) => {
    setActiveConversationId(conversation.id)
    refreshMessages(conversation.id)
  }

  const handleStartNewConversation = () => {
    setActiveConversationId(null)
    setMessages([])
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

    const assistantId = createLocalId()
    setMessages((prev) => [
      ...prev,
      { id: assistantId, role: 'assistant', content: '', pending: true },
    ])

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
        const payload = JSON.parse(event.data) as Record<string, unknown>
        const type = payload.type as string | undefined

        if (type === 'delta') {
          const delta = String(payload.content ?? '')
          if (!delta) {
            return
          }
          setMessages((prev) =>
            prev.map((item) =>
              item.id === assistantId
                ? { ...item, content: `${item.content}${delta}` }
                : item,
            ),
          )
          return
        }

        if (type === 'tool_result') {
          const tool = String(payload.tool ?? 'tool')
          const result = String(payload.result ?? '')
          setActivity((prev) => [
            {
              id: createLocalId(),
              tool,
              result,
              at: new Date().toLocaleTimeString(),
            },
            ...prev,
          ])
          return
        }

        if (type === 'done') {
          const conversationId = payload.conversation_id as string | undefined
          if (conversationId) {
            setActiveConversationId(conversationId)
            refreshConversations()
          }
          setMessages((prev) =>
            prev.map((item) =>
              item.id === assistantId ? { ...item, pending: false } : item,
            ),
          )
          ws.close()
          return
        }

        if (type === 'error') {
          setStatusMessage(String(payload.error ?? 'Streaming error'))
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
      if (
        accountForm.integration === 'google' &&
        !accountForm.refresh_token.trim()
      ) {
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
                  <span className="conversation-title">{conversation.title}</span>
                  <span className="conversation-meta">{conversation.updated_at}</span>
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
                <p>Launch conversations, stream replies, and review tool activity.</p>
              </div>
              <div className="chip-row">
                <span className={isStreaming ? 'chip active' : 'chip'}>
                  {isStreaming ? 'Streaming on' : 'Streaming off'}
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
                      Ask the assistant to orchestrate emails, calendars, and future
                      automations.
                    </p>
                  </div>
                )}
                {messages.map((message) => (
                  <div
                    key={message.id}
                    className={`message ${message.role} ${message.pending ? 'pending' : ''}`}
                  >
                    <div className="message-role">{message.role}</div>
                    <div className="message-content markdown">
                      <ReactMarkdown>{message.content}</ReactMarkdown>
                    </div>
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
                <div className="composer-actions">
                  <label className="toggle">
                    <input
                      type="checkbox"
                      checked={isStreaming}
                      onChange={(event) => setIsStreaming(event.target.checked)}
                    />
                    <span>Stream</span>
                  </label>
                  <button
                    className="button primary"
                    onClick={handleSend}
                    disabled={!canConnect || isSending}
                  >
                    {isSending ? 'Sending...' : 'Send'}
                  </button>
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
          <section className="knowledge" style={{ height: '100%', display: 'flex', flexDirection: 'column' }}>
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
            <h3>Tool activity</h3>
            <span className="muted">{activity.length} events</span>
          </div>
          <div className="activity">
            {activity.length === 0 && <p className="empty">No tool activity yet.</p>}
            {activity.map((event) => (
              <div key={event.id} className="activity-item">
                <div>
                  <p className="activity-tool">{event.tool}</p>
                  <p className="activity-time">{event.at}</p>
                </div>
                <p className="activity-result">{event.result}</p>
              </div>
            ))}
          </div>
        </div>

        <div className="card highlight">
          <h3>Next extensions</h3>
          <p className="muted">
            Use this layout to bolt on calendar views, document search, and queue
            controls as new integrations go live.
          </p>
          <div className="chip-row">
            <span className="chip">Schedules</span>
            <span className="chip">Automations</span>
            <span className="chip">Knowledge</span>
          </div>
        </div>
      </aside>
    </div>
  )
}

export default App
