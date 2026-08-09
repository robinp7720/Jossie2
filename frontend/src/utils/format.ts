export const formatDate = (value?: string | null) => {
  if (!value) return 'Not scheduled'
  const date = new Date(value)
  return Number.isNaN(date.getTime())
    ? value
    : new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeStyle: 'short' }).format(date)
}

export const relativeDate = (value?: string | null) => {
  if (!value) return 'Recently'
  const delta = new Date(value).getTime() - Date.now()
  const minutes = Math.round(delta / 60_000)
  if (Math.abs(minutes) < 60) return new Intl.RelativeTimeFormat(undefined, { numeric: 'auto' }).format(minutes, 'minute')
  const hours = Math.round(minutes / 60)
  if (Math.abs(hours) < 48) return new Intl.RelativeTimeFormat(undefined, { numeric: 'auto' }).format(hours, 'hour')
  return new Intl.RelativeTimeFormat(undefined, { numeric: 'auto' }).format(Math.round(hours / 24), 'day')
}

export const initials = (name: string) => name.split(/\s+/).map((part) => part[0]).join('').slice(0, 2).toUpperCase()
