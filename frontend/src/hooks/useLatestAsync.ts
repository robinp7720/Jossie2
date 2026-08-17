import { useCallback, useEffect, useRef, useState } from 'react'

type AsyncTaskState<T> = {
  value: T
  loading: boolean
  error: string | null
}

const errorMessage = (reason: unknown, fallback: string) =>
  reason instanceof Error ? reason.message : fallback

export function useLatestAsync<T>(initialValue: T, fallbackError: string) {
  const requestSequence = useRef(0)
  const [state, setState] = useState<AsyncTaskState<T>>({
    value: initialValue,
    loading: false,
    error: null,
  })

  useEffect(
    () => () => {
      requestSequence.current += 1
    },
    [],
  )

  const run = useCallback(
    async (
      task: () => Promise<T>,
      merge: (previous: T, next: T) => T = (_previous, next) => next,
    ) => {
      const sequence = ++requestSequence.current
      setState((previous) => ({ ...previous, loading: true, error: null }))
      try {
        const next = await task()
        if (sequence === requestSequence.current) {
          setState((previous) => ({
            value: merge(previous.value, next),
            loading: false,
            error: null,
          }))
        }
        return next
      } catch (reason) {
        if (sequence === requestSequence.current) {
          setState((previous) => ({
            ...previous,
            loading: false,
            error: errorMessage(reason, fallbackError),
          }))
        }
        return undefined
      }
    },
    [fallbackError],
  )

  const clearError = useCallback(() => {
    setState((previous) => ({ ...previous, error: null }))
  }, [])

  return { ...state, run, clearError }
}
