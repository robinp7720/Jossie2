import { useCallback, useEffect } from 'react'
import { listActivity } from '../api'
import { ActivityList } from '../components/Shared'
import { api } from '../config'
import { useLatestAsync } from '../hooks/useLatestAsync'
import type { ActivityEvent } from '../types'

export function Activity() {
  const { value, loading, error, clearError, run } = useLatestAsync<{
    items: ActivityEvent[]
    next_cursor: string | null
  }>({ items: [], next_cursor: null }, 'Unable to load activity.')
  const load = useCallback(
    (before?: string) =>
      run(
        () => listActivity(api, before),
        (previous, next) =>
          before
            ? { ...next, items: [...previous.items, ...next.items] }
            : next,
      ),
    [run],
  )
  useEffect(() => {
    void load()
  }, [load])
  return (
    <section className="page">
      <header className="page-head">
        <div>
          <p className="eyebrow">JOSSIE AT WORK</p>
          <h1>Activity.</h1>
          <p className="muted-copy">
            A clear record of completed work and meaningful updates, without
            exposing private reasoning.
          </p>
        </div>
      </header>
      {error && (
        <div className="toast-error" role="alert">
          {error}
          <button onClick={clearError}>×</button>
        </div>
      )}
      <div className="activity-page">
        <ActivityList events={value.items} expanded />
        {value.next_cursor && (
          <button
            className="button secondary"
            disabled={loading}
            onClick={() => void load(value.next_cursor ?? undefined)}
          >
            {loading ? 'Loading…' : 'Load more'}
          </button>
        )}
      </div>
    </section>
  )
}
