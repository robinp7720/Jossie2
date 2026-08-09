import { useEffect, useState } from 'react'
import { getSession } from './api'
import { api } from './config'
import { Login } from './pages/Login'
import { Workspace } from './pages/Workspace'

export default function App() {
  const [authenticated, setAuthenticated] = useState<boolean | null>(null)

  useEffect(() => {
    getSession(api).then((session) => setAuthenticated(session.authenticated)).catch(() => setAuthenticated(false))
  }, [])

  if (authenticated === null) return <div className="boot-screen">Loading Jossie…</div>
  if (!authenticated) return <Login onAuthenticated={() => setAuthenticated(true)} />
  return <Workspace onLogout={() => setAuthenticated(false)} />
}
