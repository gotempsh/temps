import { formatDistanceToNow } from 'date-fns'

interface TimeAgoProps {
  date: string | Date | number
  className?: string
}

export function TimeAgo({ date, className }: TimeAgoProps) {
  const timeAgo = formatDistanceToNow(new Date(date), { addSuffix: true })

  return <span className={className}>{timeAgo}</span>
}
