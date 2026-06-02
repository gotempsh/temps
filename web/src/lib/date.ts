import { format } from 'date-fns'

export const formatDateForAPI = (date: Date) => {
  return date.toISOString()
}

const toDate = (input: string | number | Date): Date => {
  if (input instanceof Date) return input
  return new Date(input)
}

export const formatUTCDate = (dateString: string | number) => {
  if (typeof dateString === 'number') {
    const date = new Date(dateString)
    return format(date, 'MMM d, yyyy')
  }
  const date = new Date(dateString)
  return format(date, 'MMM d, yyyy')
}

export const formatLocalDate = (
  input: string | number | Date,
  options: Intl.DateTimeFormatOptions = { dateStyle: 'medium' }
) => {
  const date = toDate(input)
  if (Number.isNaN(date.getTime())) return ''
  return new Intl.DateTimeFormat(undefined, options).format(date)
}

export const formatLocalDateTime = (
  input: string | number | Date,
  options: Intl.DateTimeFormatOptions = { dateStyle: 'medium', timeStyle: 'short' }
) => {
  const date = toDate(input)
  if (Number.isNaN(date.getTime())) return ''
  return new Intl.DateTimeFormat(undefined, options).format(date)
}

export type ExpiryRemaining = {
  expired: boolean
  totalHours: number
  totalDays: number
  short: string
  long: string
}

export const formatExpiryRemaining = (
  input: string | number | Date
): ExpiryRemaining | null => {
  const date = toDate(input)
  if (Number.isNaN(date.getTime())) return null

  const diffMs = date.getTime() - Date.now()
  const expired = diffMs <= 0
  const absMs = Math.abs(diffMs)

  const totalHours = Math.floor(absMs / (1000 * 60 * 60))
  const totalDays = Math.floor(absMs / (1000 * 60 * 60 * 24))
  const remainderHours = totalHours - totalDays * 24

  let short: string
  if (totalHours < 1) {
    const minutes = Math.max(1, Math.floor(absMs / (1000 * 60)))
    short = `${minutes}m`
  } else if (totalHours < 48) {
    short = `${totalHours}h`
  } else if (remainderHours === 0) {
    short = `${totalDays}d`
  } else {
    short = `${totalDays}d ${remainderHours}h`
  }

  const long = expired ? `Expired ${short} ago` : `Expires in ${short}`

  return { expired, totalHours, totalDays, short, long }
}
