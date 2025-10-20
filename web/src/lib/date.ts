import { formatInTimeZone } from 'date-fns-tz'
import { format } from 'date-fns'

export const formatDateForAPI = (date: Date) => {
  const utcDate = formatInTimeZone(date, 'UTC', 'yyyy-MM-dd HH:mm:ss')
  return utcDate
}

export const formatUTCDate = (dateString: string | number) => {
  if (typeof dateString === 'number') {
    const date = new Date(dateString)
    return format(date, 'MMM d, yyyy')
  }
  const date = new Date(dateString)
  return format(date, 'MMM d, yyyy')
}
