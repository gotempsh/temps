import { format } from 'date-fns'

export const formatDateForAPI = (date: Date) => {
  return date.toISOString()
}

export const formatUTCDate = (dateString: string | number) => {
  if (typeof dateString === 'number') {
    const date = new Date(dateString)
    return format(date, 'MMM d, yyyy')
  }
  const date = new Date(dateString)
  return format(date, 'MMM d, yyyy')
}
