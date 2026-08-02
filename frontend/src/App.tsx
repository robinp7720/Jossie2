import { FormEvent, useEffect, useMemo, useState } from 'react'
import ReactMarkdown from 'react-markdown'
import {
  addAccount,
  approveAction,
  buildWebSocketUrl,
  cancelConversation,
  deleteAccount,
  fetchGraph,
  getDashboard,
  getMessages,
  getSession,
  listAccounts,
  listActivity,
  listConversations,
  listMemories,
  listOnboarding,
  listPendingActions,
  login,
  logout,
  rejectAction,
  updateAccount,
  uploadFile,
} from './api'
import type { ApiConfig } from './api'
import type {
  Account,
  ActivityEvent,
  Conversation,
  Dashboard,
  GraphNode,
  Memory,
  Message,
  OnboardingStatus,
  PendingAction,
} from './types'
import { KnowledgeGraph } from './components/KnowledgeGraph'
import { AgentRunStatus } from './components/AgentRunStatus'
import type { RunStep } from './components/AgentRunStatus'

const api: ApiConfig = { baseUrl: '', token: '' }
type Page = 'overview' | 'chat' | 'memories' | 'knowledge' | 'activity' | 'connections'

const formatDate = (value?: string | null) => {
  if (!value) return 'Not scheduled'
  const date = new Date(value)
  return Number.isNaN(date.getTime())
    ? value
    : new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeStyle: 'short' }).format(date)
}

const relativeDate = (value?: string | null) => {
  if (!value) return 'Recently'
  const delta = new Date(value).getTime() - Date.now()
  const minutes = Math.round(delta / 60_000)
  if (Math.abs(minutes) < 60) return new Intl.RelativeTimeFormat(undefined, { numeric: 'auto' }).format(minutes, 'minute')
  const hours = Math.round(minutes / 60)
  if (Math.abs(hours) < 48) return new Intl.RelativeTimeFormat(undefined, { numeric: 'auto' }).format(hours, 'hour')
  return new Intl.RelativeTimeFormat(undefined, { numeric: 'auto' }).format(Math.round(hours / 24), 'day')
}

const initials = (name: string) => name.split(/\s+/).map((part) => part[0]).join('').slice(0, 2).toUpperCase()

export default function App() {
  const [authenticated, setAuthenticated] = useState<boolean | null>(null)

  useEffect(() => {
    getSession(api).then((session) => setAuthenticated(session.authenticated)).catch(() => setAuthenticated(false))
  }, [])

  if (authenticated === null) return <div className="boot-screen">Loading Jossie…</div>
  if (!authenticated) return <Login onAuthenticated={() => setAuthenticated(true)} />
  return <Workspace onLogout={() => setAuthenticated(false)} />
}

function Login({ onAuthenticated }: { onAuthenticated: () => void }) {
  const [password, setPassword] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [submitting, setSubmitting] = useState(false)

  const submit = async (event: FormEvent) => {
    event.preventDefault()
    setSubmitting(true)
    setError(null)
    try {
      await login(api, password)
      onAuthenticated()
    } catch {
      setError('The password was not accepted.')
    } finally {
      setSubmitting(false)
    }
  }

  return <main className="login-page">
    <section className="login-panel">
      <div className="brand-lockup"><span className="brand-orb">J</span><span>Jossie</span></div>
      <p className="eyebrow">PRIVATE COMPANION</p>
      <h1>Your thinking space,<br />kept private.</h1>
      <p className="muted-copy">Sign in to continue to Jossie’s memories, knowledge, and current work.</p>
      <form onSubmit={submit} className="login-form">
        <label>Password<input autoFocus type="password" value={password} onChange={(e) => setPassword(e.target.value)} autoComplete="current-password" /></label>
        {error && <p className="form-error" role="alert">{error}</p>}
        <button className="button primary full" disabled={submitting || !password}>{submitting ? 'Signing in…' : 'Enter Jossie'}</button>
      </form>
      <p className="login-note">This is a private, single-owner workspace.</p>
    </section>
    <div className="login-art" aria-hidden="true"><div className="art-ring ring-one" /><div className="art-ring ring-two" /><div className="art-core" /></div>
  </main>
}

function Workspace({ onLogout }: { onLogout: () => void }) {
  const [page, setPage] = useState<Page>('overview')
  const [dashboard, setDashboard] = useState<Dashboard | null>(null)
  const [conversations, setConversations] = useState<Conversation[]>([])
  const [error, setError] = useState<string | null>(null)

  const refresh = async () => {
    try {
      const [nextDashboard, nextConversations] = await Promise.all([getDashboard(api), listConversations(api)])
      setDashboard(nextDashboard)
      setConversations(nextConversations)
      setError(null)
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : 'Unable to load the workspace.')
    }
  }

  useEffect(() => { void refresh() }, [])
  useEffect(() => {
    const socket = new WebSocket(buildWebSocketUrl(api, '/api/events'))
    socket.onmessage = () => { void refresh() }
    return () => socket.close()
  }, [])

  const signOut = async () => {
    try { await logout(api) } finally { onLogout() }
  }

  const navigation: Array<[Page, string, string]> = [
    ['overview', 'Overview', '◌'], ['chat', 'Chat', '✦'], ['memories', 'Memories', '◫'],
    ['knowledge', 'Knowledge', '⌘'], ['activity', 'Activity', '↗'], ['connections', 'Connections', '◎'],
  ]

  return <div className="app-frame">
    <aside className="sidebar-new">
      <button className="brand-lockup brand-button" onClick={() => setPage('overview')}><span className="brand-orb">J</span><span>Jossie</span></button>
      <nav className="nav-list" aria-label="Workspace navigation">
        {navigation.map(([id, label, icon]) => <button key={id} onClick={() => setPage(id)} className={page === id ? 'nav-item selected' : 'nav-item'}><span>{icon}</span>{label}</button>)}
      </nav>
      <div className="sidebar-foot">
        <div className="status-line"><i />Private workspace</div>
        <button className="text-button" onClick={signOut}>Sign out</button>
      </div>
    </aside>
    <main className="main-stage">
      {error && <div className="toast-error">{error}<button onClick={() => setError(null)}>×</button></div>}
      {page === 'overview' && <Overview dashboard={dashboard} onNavigate={setPage} />}
      {page === 'chat' && <Chat conversations={conversations} onRefresh={refresh} />}
      {page === 'memories' && <Memories />}
      {page === 'knowledge' && <Knowledge />}
      {page === 'activity' && <Activity />}
      {page === 'connections' && <Connections />}
    </main>
  </div>
}

function Overview({ dashboard, onNavigate }: { dashboard: Dashboard | null; onNavigate: (page: Page) => void }) {
  const hour = new Date().getHours()
  const greeting = hour < 12 ? 'Good morning' : hour < 18 ? 'Good afternoon' : 'Good evening'
  if (!dashboard) return <PageLoading title={`${greeting}.`} />
  return <section className="page overview-page">
    <header className="page-head overview-head"><div><p className="eyebrow">YOUR PRIVATE COMPANION</p><h1>{greeting}.</h1><p className="muted-copy">Here’s the shape of your world in Jossie right now.</p></div><button className="button primary" onClick={() => onNavigate('chat')}>Ask Jossie <span>→</span></button></header>
    <section className="metric-grid">
      <Metric label="Memories" value={dashboard.stats.memories} detail={`${dashboard.stats.prompt_ready_memories} in active context`} mark="◫" />
      <Metric label="Knowledge" value={dashboard.stats.knowledge_nodes} detail={`${dashboard.stats.knowledge_edges} relationships`} mark="⌘" />
      <Metric label="Current work" value={dashboard.stats.pending_tasks} detail="scheduled items" mark="◌" />
      <Metric label="Recent activity" value={dashboard.recent_activity.length} detail="latest moments" mark="↗" />
    </section>
    <div className="overview-grid">
      <Panel title="What Jossie remembers" action="Browse memories" onAction={() => onNavigate('memories')} className="wide-panel">
        <div className="memory-preview-grid">{dashboard.recent_memories.length ? dashboard.recent_memories.map((memory) => <MemoryCard key={memory.key} memory={memory} compact />) : <Empty copy="No memories yet. The details that matter will begin to appear here." />}</div>
      </Panel>
      <Panel title="Recent activity" action="View timeline" onAction={() => onNavigate('activity')}>
        <ActivityList events={dashboard.recent_activity} />
      </Panel>
      <Panel title="Knowledge highlights" action="Open knowledge" onAction={() => onNavigate('knowledge')}>
        <div className="highlight-list">{dashboard.graph_highlights.length ? dashboard.graph_highlights.map(({ node, connections }) => <div className="highlight-row" key={node.id}><span className="node-avatar">{initials(node.label)}</span><div><strong>{node.label}</strong><small>{node.node_type}</small></div><span>{connections}</span></div>) : <Empty copy="The knowledge graph will grow as Jossie learns durable relationships." />}</div>
      </Panel>
      <Panel title="Coming up">
        <div className="task-list">{dashboard.upcoming_tasks.length ? dashboard.upcoming_tasks.map((task) => <div className="task-row" key={task.id}><span className="task-dot" /><div><strong>{task.task_type.replace(/_/g, ' ')}</strong><small>{task.schedule_type} · {formatDate(task.next_run_at)}</small></div></div>) : <Empty copy="No scheduled work is waiting." />}</div>
      </Panel>
    </div>
  </section>
}

function Metric({ label, value, detail, mark }: { label: string; value: number; detail: string; mark: string }) {
  return <article className="metric-card"><span className="metric-mark">{mark}</span><p>{label}</p><strong>{value}</strong><small>{detail}</small></article>
}

const contextLabelForTool = (toolName?: string | null) => {
  const name = toolName?.toLowerCase() ?? ''
  if (name.includes('memory')) return 'Saved memories'
  if (name.includes('graph')) return 'Connected knowledge'
  if (name.includes('mail') || name.includes('email') || name.includes('gmail')) return 'Email context'
  if (name.includes('calendar')) return 'Calendar context'
  if (name.includes('drive') || name.includes('file')) return 'Files and documents'
  if (name.includes('browser') || name.includes('search') || name.includes('http')) return 'External information'
  if (name.includes('schedule')) return 'Schedules'
  return 'A connected capability'
}

function Chat({ conversations, onRefresh }: { conversations: Conversation[]; onRefresh: () => Promise<void> }) {
  const [activeId, setActiveId] = useState<string | null>(conversations[0]?.id ?? null)
  const [messages, setMessages] = useState<Message[]>([])
  const [input, setInput] = useState('')
  const [sending, setSending] = useState(false)
  const [files, setFiles] = useState<Array<{ id: string; name: string }>>([])
  const [activity, setActivity] = useState<string | null>(null)
  const [pendingActions, setPendingActions] = useState<PendingAction[]>([])
  const [actionError, setActionError] = useState<string | null>(null)
  const [runSteps, setRunSteps] = useState<RunStep[]>([])
  const visibleMessages = useMemo(() => {
    const entries: Array<Message & { contextSources: string[] }> = []
    let pendingSources: string[] = []
    for (const message of messages) {
      if (message.role === 'tool') {
        pendingSources.push(contextLabelForTool(message.name))
        continue
      }
      if (message.role === 'system') continue
      if (message.role === 'user') pendingSources = []
      if (message.role === 'user' || message.role === 'assistant') {
        entries.push({
          ...message,
          contextSources: message.role === 'assistant'
            ? Array.from(new Set(pendingSources)).slice(0, 5)
            : [],
        })
        if (message.role === 'assistant') pendingSources = []
      }
    }
    return entries
  }, [messages])

  const refreshConversation = async (conversationId: string) => {
    const [nextMessages, nextActions] = await Promise.all([
      getMessages(api, conversationId, 100),
      listPendingActions(api, conversationId),
    ])
    setMessages(nextMessages)
    setPendingActions(nextActions)
  }

  useEffect(() => {
    if (!activeId && conversations[0]?.id) setActiveId(conversations[0].id)
  }, [activeId, conversations])
  useEffect(() => {
    if (activeId) void refreshConversation(activeId).catch(() => { setMessages([]); setPendingActions([]) })
    else { setMessages([]); setPendingActions([]) }
  }, [activeId])
  useEffect(() => {
    if (!activeId) return
    const events = new WebSocket(buildWebSocketUrl(api, `/api/events?conversation_id=${encodeURIComponent(activeId)}`))
    events.onmessage = (event) => {
      const payload = JSON.parse(event.data) as { type?: string }
      if (['action_approval_required', 'action_resolved', 'message_created'].includes(payload.type ?? '')) {
        void refreshConversation(activeId)
      }
    }
    return () => events.close()
  }, [activeId])

  const attach = async (file?: File) => {
    if (!file) return
    const uploaded = await uploadFile(api, file)
    setFiles((prev) => [...prev, { id: uploaded.file_id, name: uploaded.name }])
  }

  const submit = (event: FormEvent) => {
    event.preventDefault()
    const content = input.trim()
    if (!content || sending) return
    setInput(''); setSending(true); setActivity('Jossie is working…'); setActionError(null); setRunSteps([])
    setMessages((prev) => [...prev, { id: `local-${Date.now()}`, role: 'user', content, created_at: new Date().toISOString() }])
    const socket = new WebSocket(buildWebSocketUrl(api, '/api/chat/stream'))
    let conversationId = activeId
    socket.onopen = () => socket.send(JSON.stringify({ message: content, ...(conversationId ? { conversation_id: conversationId } : {}), ...(files.length ? { file_ids: files.map((file) => file.id) } : {}) }))
    socket.onmessage = (event) => {
      const payload = JSON.parse(event.data) as Record<string, unknown>
      if (payload.type === 'run_started' && typeof payload.conversation_id === 'string') { conversationId = payload.conversation_id; setActiveId(conversationId) }
      if (payload.type === 'assistant_thinking') setActivity('Jossie is considering the next step…')
      if (payload.type === 'capabilities_activated' && Array.isArray(payload.capabilities)) {
        const label = `Prepared ${payload.capabilities.join(', ')}`
        setActivity(label); setRunSteps((steps) => [...steps, { id: `cap-${steps.length}`, label, status: 'done' }])
      }
      if (payload.type === 'tool_started' && typeof payload.call_id === 'string') {
        const label = `Using ${typeof payload.tool === 'string' ? payload.tool.replace(/_/g, ' ') : 'a capability'}`
        setActivity(`${label}…`)
        setRunSteps((steps) => [...steps.filter((step) => step.id !== payload.call_id), { id: payload.call_id as string, label, status: 'running' }])
      }
      if (payload.type === 'tool_finished' && typeof payload.call_id === 'string') {
        setRunSteps((steps) => steps.map((step) => step.id === payload.call_id ? { ...step, status: payload.is_error ? 'error' : 'done' } : step))
      }
      if (payload.type === 'reflection_retry') setRunSteps((steps) => [...steps, { id: `reflection-${steps.length}`, label: 'Refined the response', status: 'done' }])
      if (payload.type === 'action_approval_required' && payload.action && typeof payload.action === 'object') {
        const action = payload.action as PendingAction
        setPendingActions((actions) => [...actions.filter((item) => item.id !== action.id), action])
      }
      const delta = payload.content
      if (payload.type === 'assistant_delta' && typeof delta === 'string') setMessages((prev) => {
        const last = prev[prev.length - 1]
        if (last?.id === 'streaming') return [...prev.slice(0, -1), { ...last, content: last.content + delta }]
        return [...prev, { id: 'streaming', role: 'assistant', content: delta, created_at: new Date().toISOString() }]
      })
      if (payload.type === 'run_waiting_for_approval') { setSending(false); setActivity('Waiting for your approval'); socket.close(); setFiles([]); if (conversationId) void refreshConversation(conversationId) }
      if (payload.type === 'pending_action') { setSending(false); setActivity(null); setActionError(typeof payload.error === 'string' ? payload.error : 'Resolve the pending action first.'); socket.close(); if (conversationId) void refreshConversation(conversationId) }
      if (payload.type === 'action_decision_received') { setSending(false); setActivity(null); socket.close(); if (conversationId) void refreshConversation(conversationId) }
      if (payload.type === 'run_completed' || payload.type === 'error' || payload.type === 'run_cancelled') { setSending(false); setActivity(null); socket.close(); setFiles([]); void onRefresh(); if (conversationId) void refreshConversation(conversationId) }
    }
    socket.onerror = () => { setSending(false); setActivity('Connection lost. Try again.'); socket.close() }
  }

  const decide = async (action: PendingAction, approve: boolean) => {
    setActionError(null)
    setPendingActions((actions) => actions.map((item) => item.id === action.id ? { ...item, status: 'executing' } : item))
    try {
      if (approve) await approveAction(api, action.id)
      else await rejectAction(api, action.id)
      if (activeId) await refreshConversation(activeId)
    } catch (reason) {
      setActionError(reason instanceof Error ? reason.message : 'Unable to resolve the action.')
      if (activeId) await refreshConversation(activeId)
    }
  }

  return <section className="page chat-page">
    <header className="page-head"><div><p className="eyebrow">CONVERSATION</p><h1>Talk with Jossie.</h1></div><button className="button secondary" onClick={() => setActiveId(null)}>New conversation</button></header>
    <div className="chat-layout">
      <aside className="thread-list"><p className="list-label">RECENT CONVERSATIONS</p>{conversations.map((conversation) => <button key={conversation.id} className={conversation.id === activeId ? 'thread selected' : 'thread'} onClick={() => setActiveId(conversation.id)}><strong>{conversation.title || 'Untitled conversation'}</strong><small>{relativeDate(conversation.updated_at)}</small></button>)}</aside>
      <div className="chat-panel">
        <div className="message-feed">{visibleMessages.length ? visibleMessages.map((message) => <article key={message.id} className={`message ${message.role}`}><span className="message-author">{message.role === 'user' ? 'You' : 'Jossie'}</span><div className="message-body"><ReactMarkdown>{message.content}</ReactMarkdown></div>{message.role === 'assistant' && message.contextSources.length > 0 && <details className="chat-context"><summary>Context used for this reply</summary><ul>{message.contextSources.map((source) => <li key={source}>{source}</li>)}</ul></details>}</article>) : <div className="chat-empty"><span className="brand-orb">J</span><h2>What’s on your mind?</h2><p>Jossie keeps the thread, the context, and the useful details together.</p></div>}</div>
        <AgentRunStatus steps={runSteps} actions={pendingActions} onDecision={(action, approve) => void decide(action, approve)} />
        {actionError && <p className="chat-action-error" role="alert">{actionError}</p>}
        <form className="composer-new" onSubmit={submit}><div className="attachment-row">{files.map((file) => <span key={file.id} className="attachment">{file.name}<button type="button" onClick={() => setFiles((prev) => prev.filter((item) => item.id !== file.id))}>×</button></span>)}</div><textarea value={input} onChange={(e) => setInput(e.target.value)} placeholder="Message Jossie…" rows={2} /><div className="composer-foot"><label className="attach-control">Attach<input type="file" onChange={(e) => void attach(e.target.files?.[0])} /></label><span>{activity}</span><button className="button primary" disabled={sending || !input.trim()}>{sending ? 'Working…' : 'Send'}</button></div></form>
        {sending && activeId && <button className="cancel-run" onClick={() => void cancelConversation(api, activeId)}>Stop current run</button>}
      </div>
    </div>
  </section>
}

function Memories() {
  const [memories, setMemories] = useState<Memory[]>([])
  const [query, setQuery] = useState('')
  const [scope, setScope] = useState('all')
  useEffect(() => { const timer = window.setTimeout(() => { void listMemories(api, query, scope).then(setMemories) }, 180); return () => clearTimeout(timer) }, [query, scope])
  return <section className="page"><header className="page-head"><div><p className="eyebrow">LONG-TERM CONTEXT</p><h1>Memories.</h1><p className="muted-copy">The durable details Jossie can bring forward when they matter.</p></div></header><div className="toolbar"><input value={query} onChange={(e) => setQuery(e.target.value)} placeholder="Search memories" /><select value={scope} onChange={(e) => setScope(e.target.value)}><option value="all">All memories</option><option value="chat">Chat context</option><option value="event">Event context</option><option value="both">Chat + event</option><option value="none">Archive only</option></select></div><div className="memory-list">{memories.map((memory) => <MemoryCard key={memory.key} memory={memory} />)}{!memories.length && <Empty copy="No memories match this view." />}</div></section>
}

function MemoryCard({ memory, compact = false }: { memory: Memory; compact?: boolean }) {
  const tags = Array.from(new Set(memory.tags.split(/[\s,]+/).filter(Boolean)))
  return <article className={compact ? 'memory-card compact' : 'memory-card'}><div className="memory-card-head"><div><p className="memory-key">{memory.key}</p>{tags.length > 0 && <div className="tag-row">{tags.map((tag) => <span key={tag}>{tag}</span>)}</div>}</div><span className="scope-badge">{memory.prompt_scope}</span></div><p>{memory.content}</p><footer><span>Importance {memory.importance}</span><span>{relativeDate(memory.updated_at)}</span></footer></article>
}

function Knowledge() {
  const [nodes, setNodes] = useState<GraphNode[]>([])
  useEffect(() => { void fetchGraph(api, 500).then((data) => setNodes(data.nodes)) }, [])
  const types = useMemo(() => new Set(nodes.map((node) => node.node_type)).size, [nodes])
  return <section className="page knowledge-page"><header className="page-head"><div><p className="eyebrow">CONNECTED CONTEXT</p><h1>Knowledge.</h1><p className="muted-copy">The people, projects, and relationships that give Jossie better context.</p></div><div className="knowledge-stats"><span>{nodes.length} entities</span><span>{types} types</span></div></header><div className="knowledge-canvas"><KnowledgeGraph apiConfig={api} /></div></section>
}

function Activity() {
  const [events, setEvents] = useState<ActivityEvent[]>([])
  const [cursor, setCursor] = useState<string | null>(null)
  const load = async (before?: string) => { const response = await listActivity(api, before); setEvents((previous) => before ? [...previous, ...response.items] : response.items); setCursor(response.next_cursor) }
  useEffect(() => { void load() }, [])
  return <section className="page"><header className="page-head"><div><p className="eyebrow">JOSSIE AT WORK</p><h1>Activity.</h1><p className="muted-copy">A clear record of completed work and meaningful updates, without exposing private reasoning.</p></div></header><div className="activity-page"><ActivityList events={events} expanded />{cursor && <button className="button secondary" onClick={() => void load(cursor)}>Load more</button>}</div></section>
}

type AccountForm = {
  integration: 'email' | 'google'
  name: string
  email: string
  username: string
  password: string
  imapHost: string
  imapPort: string
  smtpHost: string
  smtpPort: string
  refreshToken: string
}

const emptyAccountForm = (integration: AccountForm['integration'] = 'email'): AccountForm => ({
  integration,
  name: '',
  email: '',
  username: '',
  password: '',
  imapHost: '',
  imapPort: '993',
  smtpHost: '',
  smtpPort: '587',
  refreshToken: '',
})

const accountValue = (account: Account, key: string) => {
  const value = account.details?.[key]
  return typeof value === 'string' || typeof value === 'number' ? String(value) : ''
}

function Connections() {
  const [accounts, setAccounts] = useState<Account[]>([])
  const [onboarding, setOnboarding] = useState<OnboardingStatus[]>([])
  const [form, setForm] = useState<AccountForm>(emptyAccountForm())
  const [editingAccount, setEditingAccount] = useState<Account | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [saving, setSaving] = useState(false)
  const refresh = () => Promise.all([listAccounts(api).then(setAccounts), listOnboarding(api).then(setOnboarding)])

  useEffect(() => { void refresh() }, [])

  const startNew = (integration: AccountForm['integration'] = 'email') => {
    setEditingAccount(null)
    setForm(emptyAccountForm(integration))
    setError(null)
  }

  const startEdit = (account: Account) => {
    const integration = account.integration as AccountForm['integration']
    setEditingAccount(account)
    setForm({
      integration,
      name: account.name,
      email: accountValue(account, 'email'),
      username: accountValue(account, 'username'),
      password: '',
      imapHost: accountValue(account, 'imap_host'),
      imapPort: accountValue(account, 'imap_port') || '993',
      smtpHost: accountValue(account, 'smtp_host'),
      smtpPort: accountValue(account, 'smtp_port') || '587',
      refreshToken: '',
    })
    setError(null)
  }

  const submit = async (event: FormEvent) => {
    event.preventDefault()
    setSaving(true)
    setError(null)
    const config = form.integration === 'email'
      ? {
          username: form.username.trim(), password: form.password,
          imap_host: form.imapHost.trim(), imap_port: Number(form.imapPort),
          smtp_host: form.smtpHost.trim(), smtp_port: Number(form.smtpPort),
        }
      : { email: form.email.trim(), refresh_token: form.refreshToken }
    try {
      if (editingAccount) {
        await updateAccount(api, editingAccount.id, { name: form.name.trim(), config })
      } else {
        await addAccount(api, { integration: form.integration, name: form.name.trim() || `${form.integration} account`, config })
      }
      startNew()
      await refresh()
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : 'Unable to save the account.')
    } finally {
      setSaving(false)
    }
  }

  const field = <K extends keyof AccountForm,>(key: K, value: AccountForm[K]) => setForm((current) => ({ ...current, [key]: value }))
  const isEmail = form.integration === 'email'

  return <section className="page">
    <header className="page-head"><div><p className="eyebrow">CONNECTED SERVICES</p><h1>Connections.</h1><p className="muted-copy">Manage the services that let Jossie do useful work beyond the conversation.</p></div><button className="button primary" onClick={() => window.open('/setup/google', '_blank')}>Connect Google with OAuth</button></header>
    <div className="connections-grid">
      <Panel title="Connection status"><div className="integration-list">{onboarding.map((item) => <div key={item.name} className="integration-row"><span className={item.status === 'ready' ? 'ready-dot' : 'idle-dot'} /><div><strong>{item.name}</strong><small>{item.status}</small></div></div>)}</div></Panel>
      <Panel title="Saved accounts"><div className="account-list">{accounts.length ? accounts.map((account) => <div className="account-row" key={account.id}><div><strong>{account.name}</strong><small>{account.integration}{accountValue(account, 'email') ? ` · ${accountValue(account, 'email')}` : ''}</small></div><div className="account-actions"><button className="text-button" onClick={() => startEdit(account)}>Edit</button><button className="text-button danger" onClick={async () => { await deleteAccount(api, account.id); if (editingAccount?.id === account.id) startNew(); await refresh() }}>Remove</button></div></div>) : <Empty copy="No accounts configured." />}</div></Panel>
      <Panel title={editingAccount ? `Edit ${editingAccount.name}` : 'Add account'} className="add-account">
        <form className="connection-form typed-account-form" onSubmit={submit}>
          {!editingAccount && <label>Account type<select value={form.integration} onChange={(e) => startNew(e.target.value as AccountForm['integration'])}><option value="email">Email (IMAP / SMTP)</option><option value="google">Google (manual token)</option></select></label>}
          <label>Display name<input required value={form.name} onChange={(e) => field('name', e.target.value)} placeholder={isEmail ? 'Work inbox' : 'Google account'} /></label>
          {isEmail ? <>
            <label>Email or username<input required value={form.username} onChange={(e) => field('username', e.target.value)} placeholder="me@example.com" autoComplete="username" /></label>
            <label>Password<input required={!editingAccount} type="password" value={form.password} onChange={(e) => field('password', e.target.value)} placeholder={editingAccount ? 'Leave blank to keep the current password' : 'App password'} autoComplete="new-password" /></label>
            <label>IMAP host<input required value={form.imapHost} onChange={(e) => field('imapHost', e.target.value)} placeholder="imap.example.com" /></label>
            <label>IMAP port<input required inputMode="numeric" value={form.imapPort} onChange={(e) => field('imapPort', e.target.value)} /></label>
            <label>SMTP host<input required value={form.smtpHost} onChange={(e) => field('smtpHost', e.target.value)} placeholder="smtp.example.com" /></label>
            <label>SMTP port<input required inputMode="numeric" value={form.smtpPort} onChange={(e) => field('smtpPort', e.target.value)} /></label>
          </> : <>
            <label>Email address <span className="optional-label">optional</span><input type="email" value={form.email} onChange={(e) => field('email', e.target.value)} placeholder="me@gmail.com" /></label>
            <label className="form-span">Refresh token<input required={!editingAccount} type="password" value={form.refreshToken} onChange={(e) => field('refreshToken', e.target.value)} placeholder={editingAccount ? 'Leave blank to keep the current token' : 'Paste a Google refresh token'} autoComplete="new-password" /></label>
            <p className="form-hint form-span">Prefer the OAuth button above. Use a refresh token only for manual setup.</p>
          </>}
          {error && <p className="form-error form-span">{error}</p>}
          <div className="form-actions form-span"><button className="button secondary" disabled={saving}>{saving ? 'Saving…' : editingAccount ? 'Save changes' : 'Add account'}</button>{editingAccount && <button type="button" className="text-button" onClick={() => startNew()}>Cancel</button>}</div>
        </form>
      </Panel>
    </div>
  </section>
}

function Panel({ title, action, onAction, children, className = '' }: { title: string; action?: string; onAction?: () => void; children: React.ReactNode; className?: string }) {
  return <section className={`panel-new ${className}`}><div className="panel-head"><h2>{title}</h2>{action && <button className="text-button" onClick={onAction}>{action} →</button>}</div>{children}</section>
}

function ActivityList({ events, expanded = false }: { events: ActivityEvent[]; expanded?: boolean }) {
  if (!events.length) return <Empty copy="Nothing has been recorded yet." />
  return <div className={expanded ? 'activity-list expanded' : 'activity-list'}>{events.map((event) => <article className="activity-row" key={event.id}><span className={`activity-symbol ${event.tone}`}>{event.category === 'tool' ? '✦' : event.category === 'background' ? '◌' : event.category === 'reflection' ? '↻' : '·'}</span><div><strong>{event.title}</strong>{event.detail && <p>{event.detail}</p>}</div><time>{relativeDate(event.created_at)}</time></article>)}</div>
}

function Empty({ copy }: { copy: string }) { return <div className="empty-copy">{copy}</div> }
function PageLoading({ title }: { title: string }) { return <section className="page"><header className="page-head"><div><p className="eyebrow">YOUR PRIVATE COMPANION</p><h1>{title}</h1></div></header><div className="loading-lines"><i /><i /><i /></div></section> }
