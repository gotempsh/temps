import { format, formatDistanceToNow } from 'date-fns'

interface TimeAgoProps {
  date: string | Date | number
  className?: string
}

export function TimeAgo({ date, className }: TimeAgoProps) {
  const d = new Date(date)
  const timeAgo = formatDistanceToNow(d, { addSuffix: true })

  // Relative text for quick scanning; the exact local date/time is always one
  // hover away (title) for when "time ago" is too coarse to be precise.
  return (
    <span className={className} title={format(d, 'PPpp')}>
      {timeAgo}
    </span>
  )
}
