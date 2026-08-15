import { useEffect, useState } from 'react'
import { getChatImport, listMemories, startChatImport, uploadFile } from '../api'
import { Empty } from '../components/Shared'
import { api } from '../config'
import type { ChatImport, Memory } from '../types'
import { relativeDate } from '../utils/format'

export function Memories() {
  const [memories, setMemories] = useState<Memory[]>([])
  const [query, setQuery] = useState('')
  const [scope, setScope] = useState('all')
  const [refreshToken, setRefreshToken] = useState(0)
  const [exportFile, setExportFile] = useState<File | null>(null)
  const [exportFormat, setExportFormat] = useState<ChatImport['format']>('auto')
  const [chatImport, setChatImport] = useState<ChatImport | null>(null)
  const [importing, setImporting] = useState(false)
  const [importError, setImportError] = useState<string | null>(null)
  useEffect(() => { const timer = window.setTimeout(() => { void listMemories(api, query, scope).then(setMemories) }, 180); return () => clearTimeout(timer) }, [query, scope, refreshToken])

  const learnFromExport = async () => {
    if (!exportFile || importing) return
    setImporting(true); setImportError(null); setChatImport(null)
    try {
      const uploaded = await uploadFile(api, exportFile)
      let current = await startChatImport(api, uploaded.file_id, exportFormat)
      setChatImport(current)
      for (let attempt = 0; attempt < 600 && ['queued', 'processing'].includes(current.status); attempt += 1) {
        await new Promise((resolve) => window.setTimeout(resolve, 1_000))
        current = await getChatImport(api, current.id)
        setChatImport(current)
      }
      if (current.status === 'completed') {
        setRefreshToken((token) => token + 1)
        setExportFile(null)
      } else if (current.status === 'failed') {
        setImportError(current.error || 'The export could not be imported.')
      } else {
        setImportError('The import is still running. Its saved knowledge will appear here when it finishes.')
      }
    } catch (reason) {
      setImportError(reason instanceof Error ? reason.message : 'The export could not be imported.')
    } finally {
      setImporting(false)
    }
  }

  return <section className="page"><header className="page-head"><div><p className="eyebrow">LONG-TERM CONTEXT</p><h1>Memories.</h1><p className="muted-copy">The durable details Jossie can bring forward when they matter.</p></div></header><section className="import-card"><div><p className="eyebrow">LEARN FROM HISTORY</p><h2>Import a chat export</h2><p>Jossie scans the conversation in bounded chunks and saves durable, attributed facts and relationships. Chit-chat and sensitive credentials are excluded.</p></div><div className="import-controls"><label className="file-picker">{exportFile ? exportFile.name : 'Choose TXT or JSON export'}<input key={refreshToken} type="file" accept=".txt,.json,text/plain,application/json" onChange={(event) => setExportFile(event.target.files?.[0] ?? null)} /></label><select value={exportFormat} onChange={(event) => setExportFormat(event.target.value as ChatImport['format'])}><option value="auto">Auto-detect format</option><option value="whatsapp">WhatsApp text</option><option value="signal">Signal text</option><option value="chatgpt">ChatGPT conversations.json</option><option value="generic">Generic transcript/JSON</option></select><button className="button primary" disabled={!exportFile || importing} onClick={() => void learnFromExport()}>{importing ? 'Learning…' : 'Import and learn'}</button></div>{chatImport && <div className={`import-result ${chatImport.status}`}><strong>{chatImport.status === 'completed' ? 'Import complete' : chatImport.status === 'failed' ? 'Import failed' : 'Import in progress'}</strong>{chatImport.status === 'completed' && <span>{chatImport.analyzed_messages} of {chatImport.total_messages} messages analyzed · {chatImport.memories_saved} memories · {chatImport.nodes_saved} entities · {chatImport.edges_saved} relationships</span>}{chatImport.status === 'processing' && chatImport.total_messages > 0 && <span>Analyzing {chatImport.analyzed_messages} selected messages from {chatImport.total_messages}</span>}</div>}{importError && <p className="form-error" role="alert">{importError}</p>}</section><div className="toolbar"><input value={query} onChange={(e) => setQuery(e.target.value)} placeholder="Search memories" /><select value={scope} onChange={(e) => setScope(e.target.value)}><option value="all">All memories</option><option value="chat">Chat context</option><option value="event">Event context</option><option value="both">Chat + event</option><option value="none">Archive only</option></select></div><div className="memory-list">{memories.map((memory) => <MemoryCard key={memory.key} memory={memory} />)}{!memories.length && <Empty copy="No memories match this view." />}</div></section>
}

export function MemoryCard({ memory, compact = false }: { memory: Memory; compact?: boolean }) {
  const tags = Array.from(new Set(memory.tags.split(/[\s,]+/).filter(Boolean)))
  return <article className={compact ? 'memory-card compact' : 'memory-card'}><div className="memory-card-head"><div><p className="memory-key">{memory.key}</p>{tags.length > 0 && <div className="tag-row">{tags.map((tag) => <span key={tag}>{tag}</span>)}</div>}</div><span className="scope-badge">{memory.prompt_scope}</span></div><p>{memory.content}</p><footer><span>Importance {memory.importance}</span><span>{relativeDate(memory.updated_at)}</span></footer></article>
}
