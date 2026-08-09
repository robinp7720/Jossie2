import { afterEach, describe, expect, it, vi } from 'vitest'
import { formatDate, initials, relativeDate } from './format'

afterEach(() => vi.useRealTimers())

describe('format utilities', () => {
  it('handles missing and invalid dates without throwing', () => {
    expect(formatDate()).toBe('Not scheduled')
    expect(formatDate('not-a-date')).toBe('not-a-date')
  })

  it('formats relative dates against the current clock', () => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date('2026-08-08T12:00:00Z'))
    expect(relativeDate('2026-08-08T11:55:00Z')).toContain('5 minutes ago')
  })

  it('builds compact initials', () => {
    expect(initials('Jossie Companion')).toBe('JC')
  })
})
