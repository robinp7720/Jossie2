import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import type { FormEvent, KeyboardEvent } from 'react'
import ReactMarkdown from 'react-markdown'
import {
  approveAction, buildWebSocketUrl, cancelConversation, createConversation,
  deleteConversation, deleteUploadedFile, downloadConversationExport, downloadUploadedFile,
  getMessages, getWork, listConversations, listPendingActions, rejectAction,
  updateConversation, uploadFile,
} from '../api'
import { AgentRunStatus } from '../components/AgentRunStatus'
import type { RunStep } from '../components/AgentRunStatus'
import { api } from '../config'
import { useWorkspaceEvents } from '../events'
import type { Conversation, Message, PendingAction, WorkRun } from '../types'
import { relativeDate } from '../utils/format'

type ThreadView = 'active' | 'archived'
type DraftFile = { localId: string; id?: string; name: string; file: File; status: 'uploading' | 'ready' | 'error'; error?: string }

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

export function Chat({ conversations: initialConversations, onRefresh }: { conversations: Conversation[]; onRefresh: () => Promise<void> }) {
  const [view, setView] = useState<ThreadView>('active')
  const [query, setQuery] = useState('')
  const [threads, setThreads] = useState(initialConversations)
  const [hasMoreThreads, setHasMoreThreads] = useState(initialConversations.length >= 50)
  const [activeId, setActiveId] = useState<string | null>(initialConversations[0]?.id ?? null)
  const [focusMessageId, setFocusMessageId] = useState<string | null>(null)
  const [messages, setMessages] = useState<Message[]>([])
  const [hasOlder, setHasOlder] = useState(false)
  const [input, setInput] = useState('')
  const [sending, setSending] = useState(false)
  const [files, setFiles] = useState<DraftFile[]>([])
  const [activity, setActivity] = useState<string | null>(null)
  const [pendingActions, setPendingActions] = useState<PendingAction[]>([])
  const [actionError, setActionError] = useState<string | null>(null)
  const [runSteps, setRunSteps] = useState<RunStep[]>([])
  const [activeRuns, setActiveRuns] = useState<WorkRun[]>([])
  const { event: workspaceEvent, sequence: eventSequence, connection } = useWorkspaceEvents()
  const [renaming, setRenaming] = useState(false)
  const [renameTitle, setRenameTitle] = useState('')
  const [confirmDelete, setConfirmDelete] = useState<Conversation | null>(null)
  const [showJump, setShowJump] = useState(false)
  const feedRef = useRef<HTMLDivElement>(null)
  const threadsRef = useRef(initialConversations)
  const directSocketActive = useRef(false)
  const acceptedRef = useRef(false)
  const retryMessageId = useRef<string | null>(null)
  const activeConversation = threads.find((item) => item.id === activeId) ?? initialConversations.find((item) => item.id === activeId)

  useEffect(() => { threadsRef.current = threads }, [threads])

  const visibleMessages = useMemo(() => {
    const entries: Array<Message & { contextSources: string[] }> = []
    let pendingSources: string[] = []
    for (const message of messages) {
      if (message.role === 'tool') { pendingSources.push(contextLabelForTool(message.name)); continue }
      if (message.role === 'system') continue
      if (message.role === 'user') pendingSources = []
      entries.push({ ...message, contextSources: message.role === 'assistant' ? Array.from(new Set(pendingSources)).slice(0, 5) : [] })
      if (message.role === 'assistant') pendingSources = []
    }
    return entries
  }, [messages])

  const loadThreads = useCallback(async (append = false) => {
    const before = append ? threadsRef.current.at(-1)?.id : undefined
    const next = await listConversations(api, { view, q: query.trim() || undefined, limit: 50, before })
    setThreads((current) => append ? [...current, ...next.filter((item) => !current.some((old) => old.id === item.id))] : next)
    setHasMoreThreads(next.length === 50)
    return next
  }, [query, view])

  useEffect(() => {
    const timer = window.setTimeout(() => { void loadThreads().catch((reason) => setActionError(reason instanceof Error ? reason.message : 'Unable to load conversations.')) }, 180)
    return () => clearTimeout(timer)
  }, [query, view])

  const refreshConversation = useCallback(async (conversationId: string, around?: string | null) => {
    const [nextMessages, nextActions, nextWork] = await Promise.all([
      getMessages(api, conversationId, 100, around ? { around } : {}),
      listPendingActions(api, conversationId), getWork(api, conversationId),
    ])
    setMessages(nextMessages); setHasOlder(nextMessages.length === 100)
    setPendingActions(nextActions); setActiveRuns(nextWork.active_runs)
    setSending(nextWork.active_runs.some((run) => ['queued', 'running'].includes(run.status)))
  }, [])

  useEffect(() => {
    if (activeId) void refreshConversation(activeId, focusMessageId).catch((reason) => setActionError(reason instanceof Error ? reason.message : 'Unable to load conversation.'))
    else { setMessages([]); setPendingActions([]); setActiveRuns([]); setHasOlder(false) }
  }, [activeId, focusMessageId, refreshConversation])

  const applyRunEvent = useCallback((payload: Record<string, unknown>, allowDelta: boolean) => {
    if (typeof payload.conversation_id === 'string' && activeId && payload.conversation_id !== activeId) return
    if (payload.type === 'run_started') { setSending(true); setActivity('Jossie is working…') }
    if (payload.type === 'work_run_updated' && payload.run && typeof payload.run === 'object') {
      const run = payload.run as WorkRun
      setActiveRuns((runs) => ['queued', 'running', 'waiting_for_approval'].includes(run.status) ? [...runs.filter((item) => item.id !== run.id), run] : runs.filter((item) => item.id !== run.id))
    }
    if (payload.type === 'assistant_thinking') setActivity('Jossie is considering the next step…')
    if (payload.type === 'capabilities_activated' && Array.isArray(payload.capabilities)) {
      const label = `Prepared ${payload.capabilities.join(', ')}`
      setActivity(label); setRunSteps((steps) => [...steps, { id: `cap-${steps.length}`, label, status: 'done' }])
    }
    if (payload.type === 'tool_started' && typeof payload.call_id === 'string') {
      const label = `Using ${typeof payload.tool === 'string' ? payload.tool.replace(/_/g, ' ') : 'a capability'}`
      setActivity(`${label}…`); setRunSteps((steps) => [...steps.filter((step) => step.id !== payload.call_id), { id: payload.call_id as string, label, status: 'running' }])
    }
    if (payload.type === 'tool_finished' && typeof payload.call_id === 'string') setRunSteps((steps) => steps.map((step) => step.id === payload.call_id ? { ...step, status: payload.is_error ? 'error' : 'done' } : step))
    if (payload.type === 'reflection_retry') setRunSteps((steps) => [...steps, { id: `reflection-${steps.length}`, label: 'Refined the response', status: 'done' }])
    if (payload.type === 'action_approval_required' && payload.action && typeof payload.action === 'object') {
      const action = payload.action as PendingAction
      setPendingActions((actions) => [...actions.filter((item) => item.id !== action.id), action])
    }
    if (allowDelta && payload.type === 'assistant_delta' && typeof payload.content === 'string') {
      const streamId = `stream-${String(payload.run_id ?? 'active')}`
      setMessages((current) => current.some((message) => message.id === streamId)
        ? current.map((message) => message.id === streamId ? { ...message, content: message.content + payload.content } : message)
        : [...current, { id: streamId, role: 'assistant', content: payload.content as string, created_at: new Date().toISOString() }])
    }
    if (['run_waiting_for_approval', 'run_completed', 'run_paused', 'run_cancelled', 'error'].includes(String(payload.type))) {
      setSending(false); setActivity(payload.type === 'run_waiting_for_approval' ? 'Waiting for your approval' : null)
      if (activeId) void refreshConversation(activeId)
      void loadThreads(); void onRefresh()
    }
    if (['message_created', 'action_resolved', 'goal_updated'].includes(String(payload.type)) && activeId) void refreshConversation(activeId)
  }, [activeId, loadThreads, onRefresh, refreshConversation])

  useEffect(() => {
    if (workspaceEvent) applyRunEvent(workspaceEvent as unknown as Record<string, unknown>, !directSocketActive.current)
  }, [eventSequence, workspaceEvent, applyRunEvent])

  useEffect(() => {
    const feed = feedRef.current
    if (!feed) return
    const nearBottom = feed.scrollHeight - feed.scrollTop - feed.clientHeight < 140
    if (nearBottom) { feed.scrollTop = feed.scrollHeight; setShowJump(false) } else setShowJump(true)
  }, [visibleMessages.length, visibleMessages.at(-1)?.content])

  useEffect(() => { if (focusMessageId) window.setTimeout(() => document.getElementById(`message-${focusMessageId}`)?.scrollIntoView({ block: 'center' }), 0) }, [focusMessageId, messages])

  const uploadDraft = async (draft: DraftFile) => {
    setFiles((current) => current.map((item) => item.localId === draft.localId ? { ...item, status: 'uploading', error: undefined } : item))
    try {
      const uploaded = await uploadFile(api, draft.file)
      setFiles((current) => current.map((item) => item.localId === draft.localId ? { ...item, id: uploaded.file_id, status: 'ready' } : item))
    } catch (reason) {
      setFiles((current) => current.map((item) => item.localId === draft.localId ? { ...item, status: 'error', error: reason instanceof Error ? reason.message : 'Upload failed' } : item))
    }
  }

  const attach = (selected: FileList | null) => {
    for (const file of Array.from(selected ?? [])) {
      const draft: DraftFile = { localId: crypto.randomUUID(), name: file.name, file, status: 'uploading' }
      setFiles((current) => [...current, draft]); void uploadDraft(draft)
    }
  }

  const removeDraft = async (draft: DraftFile) => {
    setFiles((current) => current.filter((item) => item.localId !== draft.localId))
    if (draft.id) await deleteUploadedFile(api, draft.id).catch(() => undefined)
  }

  const submit = async (event?: FormEvent) => {
    event?.preventDefault()
    const content = input.trim()
    if (!content || sending || files.some((file) => file.status !== 'ready')) return
    setActionError(null); setRunSteps([]); acceptedRef.current = false
    let conversationId = activeId
    try {
      if (!conversationId) {
        const created = await createConversation(api)
        conversationId = created.id; setActiveId(created.id); setThreads((current) => [created, ...current])
      }
      const messageId = retryMessageId.current ?? crypto.randomUUID()
      retryMessageId.current = messageId
      setSending(true); setActivity('Sending your message…')
      setMessages((current) => current.some((message) => message.id === messageId) ? current : [...current, {
        id: messageId, role: 'user', content, created_at: new Date().toISOString(),
        attachments: files.filter((file) => file.id).map((file) => ({ id: file.id!, name: file.name, size: file.file.size, mime_type: file.file.type })),
      }])
      const socket = new WebSocket(buildWebSocketUrl(api, '/api/chat/stream'))
      let terminalHandled = false
      directSocketActive.current = true
      socket.onopen = () => socket.send(JSON.stringify({ message: content, conversation_id: conversationId, client_message_id: messageId, ...(files.length ? { file_ids: files.map((file) => file.id) } : {}) }))
      socket.onmessage = (messageEvent) => {
        const payload = JSON.parse(messageEvent.data) as Record<string, unknown>
        if (payload.type === 'message_accepted') {
          acceptedRef.current = true; retryMessageId.current = null; setInput(''); setFiles([]); setActivity('Jossie is working…')
        } else if (payload.type === 'pending_action') {
          terminalHandled = true; setSending(false); setActivity(null); setActionError(typeof payload.error === 'string' ? payload.error : 'Resolve the pending action first.')
        } else if (payload.type === 'action_decision_received') {
          terminalHandled = true; setSending(false); setActivity(null); setInput(''); retryMessageId.current = null
        } else {
          if (payload.type === 'error' && typeof payload.error === 'string') { terminalHandled = true; setActionError(payload.error) }
          applyRunEvent(payload, true)
        }
        if (['run_waiting_for_approval', 'run_completed', 'run_paused', 'run_cancelled', 'error', 'pending_action', 'action_decision_received'].includes(String(payload.type))) socket.close()
      }
      socket.onclose = () => {
        directSocketActive.current = false
        if (!acceptedRef.current && !terminalHandled) {
          setSending(false); setActivity(null); setActionError('The connection closed before the message was confirmed. Retry will safely reuse the same message.')
        } else if (conversationId) void refreshConversation(conversationId)
      }
      socket.onerror = () => socket.close()
    } catch (reason) {
      setSending(false); setActivity(null); setActionError(reason instanceof Error ? reason.message : 'Unable to send message.')
    }
  }

  const loadOlder = async () => {
    if (!activeId || !messages.length) return
    const older = await getMessages(api, activeId, 100, { before: messages[0].id })
    setMessages((current) => [...older, ...current]); setHasOlder(older.length === 100)
  }

  const chooseThread = (conversation: Conversation) => {
    setActiveId(conversation.id); setFocusMessageId(conversation.matched_message_id ?? null); setRenaming(false); setActionError(null)
  }

  const archive = async (conversation: Conversation, archived: boolean) => {
    try {
      await updateConversation(api, conversation.id, { archived })
      const next = await loadThreads()
      if (activeId === conversation.id) setActiveId(next.find((item) => item.id !== conversation.id)?.id ?? null)
      void onRefresh()
    } catch (reason) { setActionError(reason instanceof Error ? reason.message : 'Unable to update conversation.') }
  }

  const saveRename = async () => {
    if (!activeConversation || !renameTitle.trim()) return
    try { await updateConversation(api, activeConversation.id, { title: renameTitle.trim() }); setRenaming(false); await loadThreads(); void onRefresh() }
    catch (reason) { setActionError(reason instanceof Error ? reason.message : 'Unable to rename conversation.') }
  }

  const permanentlyDelete = async () => {
    if (!confirmDelete) return
    const deleting = confirmDelete
    try {
      await deleteConversation(api, deleting.id); setConfirmDelete(null)
      const next = await loadThreads()
      if (activeId === deleting.id) setActiveId(next[0]?.id ?? null)
      void onRefresh()
    } catch (reason) { setActionError(reason instanceof Error ? reason.message : 'Unable to delete conversation.'); setConfirmDelete(null) }
  }

  const decide = async (action: PendingAction, approve: boolean) => {
    setActionError(null); setPendingActions((actions) => actions.map((item) => item.id === action.id ? { ...item, status: 'executing' } : item))
    try { approve ? await approveAction(api, action.id) : await rejectAction(api, action.id); if (activeId) await refreshConversation(activeId) }
    catch (reason) { setActionError(reason instanceof Error ? reason.message : 'Unable to resolve the action.'); if (activeId) await refreshConversation(activeId) }
  }

  const composerKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key === 'Enter' && !event.shiftKey) { event.preventDefault(); void submit() }
  }

  return <section className="page chat-page">
    <header className="page-head"><div><p className="eyebrow">CONVERSATION</p><h1>Talk with Jossie.</h1></div><button className="button secondary" onClick={() => { setView('active'); setActiveId(null); setFocusMessageId(null); setMessages([]) }}>New conversation</button></header>
    <div className="chat-layout">
      <aside className="thread-list">
        <div className="thread-search"><input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="Search conversations" aria-label="Search conversations" /><div className="thread-tabs" role="tablist"><button className={view === 'active' ? 'selected' : ''} onClick={() => setView('active')}>Active</button><button className={view === 'archived' ? 'selected' : ''} onClick={() => setView('archived')}>Archived</button></div></div>
        {threads.map((conversation) => <button key={conversation.id} className={conversation.id === activeId ? 'thread selected' : 'thread'} onClick={() => chooseThread(conversation)}><strong>{conversation.title || 'Untitled conversation'}</strong>{conversation.preview && <span>{conversation.preview}</span>}<small>{relativeDate(conversation.updated_at)}{conversation.message_count != null ? ` · ${conversation.message_count} messages` : ''}</small></button>)}
        {!threads.length && <p className="thread-empty">No conversations match this view.</p>}
        {hasMoreThreads && <button className="text-button thread-more" onClick={() => void loadThreads(true)}>Load more</button>}
      </aside>
      <div className="chat-panel">
        {activeConversation && <div className="conversation-bar">
          {renaming ? <div className="rename-row"><input value={renameTitle} maxLength={120} onChange={(event) => setRenameTitle(event.target.value)} autoFocus /><button className="text-button" onClick={() => void saveRename()}>Save</button><button className="text-button" onClick={() => setRenaming(false)}>Cancel</button></div> : <strong>{activeConversation.title || 'Untitled conversation'}</strong>}
          <div className="conversation-actions">
            <button className="text-button" onClick={() => { setRenameTitle(activeConversation.title ?? ''); setRenaming(true) }}>Rename</button>
            <button className="text-button" onClick={() => void downloadConversationExport(api, activeConversation.id, 'markdown')}>Export Markdown</button>
            <button className="text-button" onClick={() => void downloadConversationExport(api, activeConversation.id, 'json')}>Export JSON</button>
            {view === 'active' ? <button className="text-button" onClick={() => void archive(activeConversation, true)}>Archive</button> : <><button className="text-button" onClick={() => void archive(activeConversation, false)}>Restore</button><button className="text-button danger" onClick={() => setConfirmDelete(activeConversation)}>Delete permanently</button></>}
          </div>
        </div>}
        <div className="message-feed" ref={feedRef} onScroll={() => { const feed = feedRef.current; if (feed && feed.scrollHeight - feed.scrollTop - feed.clientHeight < 140) setShowJump(false) }}>
          {hasOlder && <button className="text-button load-older" onClick={() => void loadOlder()}>Load older messages</button>}
          {visibleMessages.length ? visibleMessages.map((message) => <article id={`message-${message.id}`} key={message.id} className={`message ${message.role}${message.id === focusMessageId ? ' focused' : ''}`}><span className="message-author">{message.role === 'user' ? 'You' : 'Jossie'}</span><div className="message-body"><ReactMarkdown>{message.content}</ReactMarkdown></div>{message.attachments?.length ? <div className="message-attachments">{message.attachments.map((attachment) => <button type="button" key={attachment.id} onClick={() => void downloadUploadedFile(api, attachment.id, attachment.name).catch((reason) => setActionError(reason instanceof Error ? reason.message : 'Unable to download attachment.'))}>{attachment.name}<small>{Math.ceil(attachment.size / 1024)} KB</small></button>)}</div> : null}{message.role === 'assistant' && message.contextSources.length > 0 && <details className="chat-context"><summary>Context used for this reply</summary><ul>{message.contextSources.map((source) => <li key={source}>{source}</li>)}</ul></details>}</article>) : <div className="chat-empty"><span className="brand-orb">J</span><h2>What’s on your mind?</h2><p>Jossie keeps the thread, the context, and the useful details together.</p></div>}
        </div>
        {showJump && <button className="jump-latest" onClick={() => { if (feedRef.current) feedRef.current.scrollTop = feedRef.current.scrollHeight; setShowJump(false) }}>New messages ↓</button>}
        <AgentRunStatus steps={runSteps} runs={activeRuns} actions={pendingActions} onDecision={(action, approve) => void decide(action, approve)} />
        {actionError && <p className="chat-action-error" role="alert">{actionError}</p>}
        <form className="composer-new" onSubmit={(event) => void submit(event)}>
          <div className="attachment-row">{files.map((file) => <span key={file.localId} className={`attachment ${file.status}`}>{file.name}<small>{file.status === 'uploading' ? 'Uploading…' : file.status === 'error' ? file.error : 'Ready'}</small>{file.status === 'error' && <button type="button" onClick={() => void uploadDraft(file)}>Retry</button>}<button type="button" aria-label={`Remove ${file.name}`} onClick={() => void removeDraft(file)}>×</button></span>)}</div>
          <textarea value={input} onChange={(event) => { setInput(event.target.value); if (retryMessageId.current) retryMessageId.current = null }} onKeyDown={composerKeyDown} placeholder="Message Jossie…" rows={2} />
          <div className="composer-foot"><label className="attach-control">Attach<input type="file" multiple onChange={(event) => { attach(event.target.files); event.target.value = '' }} /></label><span>{connection === 'reconnecting' ? 'Reconnecting—Jossie keeps working…' : activity}</span><button className="button primary" disabled={sending || !input.trim() || files.some((file) => file.status !== 'ready')}>{sending ? 'Working…' : 'Send'}</button></div>
        </form>
        {sending && activeId && <button className="cancel-run" onClick={() => void cancelConversation(api, activeId)}>Stop current run</button>}
      </div>
    </div>
    {confirmDelete && <div className="dialog-backdrop" role="presentation"><section className="confirm-dialog" role="dialog" aria-modal="true" aria-labelledby="delete-conversation-title"><h2 id="delete-conversation-title">Delete this conversation permanently?</h2><p>The transcript, conversation-specific work, schedules, and exclusive attachments will be removed. Memories and knowledge Jossie learned are not removed.</p><footer><button className="button secondary" onClick={() => setConfirmDelete(null)}>Keep conversation</button><button className="button danger" onClick={() => void permanentlyDelete()}>Delete permanently</button></footer></section></div>}
  </section>
}
