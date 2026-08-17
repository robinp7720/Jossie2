import { act, renderHook } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import { useLatestAsync } from './useLatestAsync'

const deferred = <T,>() => {
  let resolve!: (value: T) => void
  const promise = new Promise<T>((next) => {
    resolve = next
  })
  return { promise, resolve }
}

describe('useLatestAsync', () => {
  it('ignores an older request that finishes last', async () => {
    const first = deferred<string>()
    const second = deferred<string>()
    const { result } = renderHook(() => useLatestAsync('', 'Failed'))

    let firstRun!: Promise<string | undefined>
    let secondRun!: Promise<string | undefined>
    act(() => {
      firstRun = result.current.run(() => first.promise)
      secondRun = result.current.run(() => second.promise)
    })
    await act(async () => {
      second.resolve('new')
      await secondRun
    })
    await act(async () => {
      first.resolve('old')
      await firstRun
    })

    expect(result.current.value).toBe('new')
    expect(result.current.loading).toBe(false)
  })

  it('normalizes rejected tasks into visible state', async () => {
    const { result } = renderHook(() => useLatestAsync([], 'Unable to load'))

    await act(async () => {
      await result.current.run(() => Promise.reject(new Error('Offline')))
    })

    expect(result.current.error).toBe('Offline')
    expect(result.current.loading).toBe(false)
  })
})
