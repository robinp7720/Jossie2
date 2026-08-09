import { useEffect, useState } from 'react'
import { listActivity } from '../api'
import { ActivityList } from '../components/Shared'
import { api } from '../config'
import type { ActivityEvent } from '../types'

export function Activity() {
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

