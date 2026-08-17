import { createContext, useContext, useEffect, useMemo, useState } from 'react'
import type { ReactNode } from 'react'
import { buildWebSocketUrl } from './api'
import type { ApiConfig } from './api'
import type { ServerEvent } from './types'

type EventConnection = 'online' | 'reconnecting'
type WorkspaceEvents = {
  event: ServerEvent | null
  sequence: number
  connection: EventConnection
}

const EventContext = createContext<WorkspaceEvents>({
  event: null,
  sequence: 0,
  connection: 'online',
})

export function WorkspaceEventProvider({
  api,
  children,
}: {
  api: ApiConfig
  children: ReactNode
}) {
  const [event, setEvent] = useState<ServerEvent | null>(null)
  const [sequence, setSequence] = useState(0)
  const [connection, setConnection] = useState<EventConnection>('online')

  useEffect(() => {
    let stopped = false
    let socket: WebSocket | null = null
    let retryTimer = 0
    let attempt = 0

    const connect = () => {
      socket = new WebSocket(buildWebSocketUrl(api, '/api/events'))
      socket.onopen = () => {
        attempt = 0
        setConnection('online')
      }
      socket.onmessage = (message) => {
        try {
          const payload = JSON.parse(message.data) as ServerEvent
          if (
            !payload ||
            typeof payload !== 'object' ||
            typeof payload.type !== 'string'
          )
            return
          setEvent(payload)
          setSequence((current) => current + 1)
        } catch {
          /* Ignore malformed server frames. */
        }
      }
      socket.onclose = () => {
        if (stopped) return
        setConnection('reconnecting')
        attempt += 1
        retryTimer = window.setTimeout(
          connect,
          Math.min(10_000, 500 * 2 ** attempt),
        )
      }
      socket.onerror = () => socket?.close()
    }

    connect()
    return () => {
      stopped = true
      socket?.close()
      window.clearTimeout(retryTimer)
    }
  }, [api])

  const value = useMemo(
    () => ({ event, sequence, connection }),
    [event, sequence, connection],
  )
  return <EventContext.Provider value={value}>{children}</EventContext.Provider>
}

export const useWorkspaceEvents = () => useContext(EventContext)
