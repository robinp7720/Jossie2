import { useEffect, useMemo, useState } from 'react'
import type { FormEvent } from 'react'
import ReactMarkdown from 'react-markdown'
import { approveAction, buildWebSocketUrl, cancelConversation, getMessages, getWork, listPendingActions, rejectAction, uploadFile } from '../api'
import { AgentRunStatus } from '../components/AgentRunStatus'
import type { RunStep } from '../components/AgentRunStatus'
import { api } from '../config'
import type { Conversation, Message, PendingAction, WorkRun } from '../types'
import { relativeDate } from '../utils/format'

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

export function Chat({ conversations, onRefresh }: { conversations: Conversation[]; onRefresh: () => Promise<void> }) {
  const [activeId, setActiveId] = useState<string | null>(conversations[0]?.id ?? null)
  const [messages, setMessages] = useState<Message[]>([])
  const [input, setInput] = useState('')
  const [sending, setSending] = useState(false)
  const [files, setFiles] = useState<Array<{ id: string; name: string }>>([])
  const [activity, setActivity] = useState<string | null>(null)
  const [pendingActions, setPendingActions] = useState<PendingAction[]>([])
  const [actionError, setActionError] = useState<string | null>(null)
  const [runSteps, setRunSteps] = useState<RunStep[]>([])
  const [activeRuns, setActiveRuns] = useState<WorkRun[]>([])
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
    const [nextMessages, nextActions, nextWork] = await Promise.all([
      getMessages(api, conversationId, 100),
      listPendingActions(api, conversationId),
      getWork(api, conversationId),
    ])
    setMessages(nextMessages)
    setPendingActions(nextActions)
    setActiveRuns(nextWork.active_runs)
  }

  useEffect(() => {
    if (!activeId && conversations[0]?.id) setActiveId(conversations[0].id)
  }, [activeId, conversations])
  useEffect(() => {
    if (activeId) void refreshConversation(activeId).catch(() => { setMessages([]); setPendingActions([]) })
    else { setMessages([]); setPendingActions([]); setActiveRuns([]) }
  }, [activeId])
  useEffect(() => {
    if (!activeId) return
    const events = new WebSocket(buildWebSocketUrl(api, `/api/events?conversation_id=${encodeURIComponent(activeId)}`))
    events.onmessage = (event) => {
      const payload = JSON.parse(event.data) as { type?: string }
      if (['action_approval_required', 'action_resolved', 'message_created', 'work_run_updated', 'work_step_updated', 'goal_updated'].includes(payload.type ?? '')) {
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
      if (payload.type === 'work_run_updated' && payload.run && typeof payload.run === 'object') {
        const run = payload.run as WorkRun
        setActiveRuns((runs) => ['queued', 'running', 'waiting_for_approval'].includes(run.status)
          ? [...runs.filter((item) => item.id !== run.id), run]
          : runs.filter((item) => item.id !== run.id))
      }
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
      if (payload.type === 'run_completed' || payload.type === 'run_paused' || payload.type === 'error' || payload.type === 'run_cancelled') { setSending(false); setActivity(null); socket.close(); setFiles([]); void onRefresh(); if (conversationId) void refreshConversation(conversationId) }
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
        <AgentRunStatus steps={runSteps} runs={activeRuns} actions={pendingActions} onDecision={(action, approve) => void decide(action, approve)} />
        {actionError && <p className="chat-action-error" role="alert">{actionError}</p>}
        <form className="composer-new" onSubmit={submit}><div className="attachment-row">{files.map((file) => <span key={file.id} className="attachment">{file.name}<button type="button" onClick={() => setFiles((prev) => prev.filter((item) => item.id !== file.id))}>×</button></span>)}</div><textarea value={input} onChange={(e) => setInput(e.target.value)} placeholder="Message Jossie…" rows={2} /><div className="composer-foot"><label className="attach-control">Attach<input type="file" onChange={(e) => void attach(e.target.files?.[0])} /></label><span>{activity}</span><button className="button primary" disabled={sending || !input.trim()}>{sending ? 'Working…' : 'Send'}</button></div></form>
        {sending && activeId && <button className="cancel-run" onClick={() => void cancelConversation(api, activeId)}>Stop current run</button>}
      </div>
    </div>
  </section>
}

