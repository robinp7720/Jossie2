import { useCallback, useEffect, useState } from 'react'
import { getDashboard, listConversations, logout } from '../api'
import { WorkPage } from '../components/WorkPage'
import { api } from '../config'
import { useWorkspaceEvents, WorkspaceEventProvider } from '../events'
import type { Page } from '../navigation'
import type { Conversation, Dashboard } from '../types'
import { Activity } from './Activity'
import { Chat } from './Chat'
import { Connections } from './Connections'
import { Knowledge } from './Knowledge'
import { Memories } from './Memories'
import { Overview } from './Overview'

function WorkspaceContent({ onLogout }: { onLogout: () => void }) {
  const [page, setPage] = useState<Page>('overview')
  const [dashboard, setDashboard] = useState<Dashboard | null>(null)
  const [conversations, setConversations] = useState<Conversation[]>([])
  const [error, setError] = useState<string | null>(null)
  const { sequence } = useWorkspaceEvents()

  const refresh = useCallback(async () => {
    try {
      const [nextDashboard, nextConversations] = await Promise.all([
        getDashboard(api),
        listConversations(api),
      ])
      setDashboard(nextDashboard)
      setConversations(nextConversations)
      setError(null)
    } catch (reason) {
      setError(
        reason instanceof Error
          ? reason.message
          : 'Unable to load the workspace.',
      )
    }
  }, [])

  useEffect(() => {
    void refresh()
  }, [refresh])
  useEffect(() => {
    if (sequence) void refresh()
  }, [refresh, sequence])

  const signOut = async () => {
    try {
      await logout(api)
    } finally {
      onLogout()
    }
  }

  const navigation: Array<[Page, string, string]> = [
    ['overview', 'Overview', '◌'],
    ['work', 'Work', '◉'],
    ['chat', 'Chat', '✦'],
    ['memories', 'Memories', '◫'],
    ['knowledge', 'Knowledge', '⌘'],
    ['activity', 'Activity', '↗'],
    ['connections', 'Connections', '◎'],
  ]

  return (
    <div className="app-frame">
      <aside className="sidebar-new">
        <button
          className="brand-lockup brand-button"
          onClick={() => setPage('overview')}
        >
          <span className="brand-orb">J</span>
          <span>Jossie</span>
        </button>
        <nav className="nav-list" aria-label="Workspace navigation">
          {navigation.map(([id, label, icon]) => (
            <button
              key={id}
              onClick={() => setPage(id)}
              className={page === id ? 'nav-item selected' : 'nav-item'}
            >
              <span>{icon}</span>
              {label}
            </button>
          ))}
        </nav>
        <div className="sidebar-foot">
          <div className="status-line">
            <i />
            Private workspace
          </div>
          <button className="text-button" onClick={signOut}>
            Sign out
          </button>
        </div>
      </aside>
      <main className="main-stage">
        {error && (
          <div className="toast-error">
            {error}
            <button onClick={() => setError(null)}>×</button>
          </div>
        )}
        {page === 'overview' && (
          <Overview dashboard={dashboard} onNavigate={setPage} />
        )}
        {page === 'work' && <WorkPage api={api} />}
        {page === 'chat' && (
          <Chat conversations={conversations} onRefresh={refresh} />
        )}
        {page === 'memories' && <Memories />}
        {page === 'knowledge' && <Knowledge />}
        {page === 'activity' && <Activity />}
        {page === 'connections' && <Connections />}
      </main>
    </div>
  )
}

export function Workspace({ onLogout }: { onLogout: () => void }) {
  return (
    <WorkspaceEventProvider api={api}>
      <WorkspaceContent onLogout={onLogout} />
    </WorkspaceEventProvider>
  )
}
