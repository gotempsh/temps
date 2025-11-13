import { memo } from 'react'
import { cn } from '@/lib/utils'

interface LogLineProps {
  content: string
  isHighlighted?: boolean
  searchTerm?: string
}

export const LogLine = memo(function LogLine({
  content,
  isHighlighted,
  searchTerm,
}: LogLineProps) {
  const highlightSearchTerm = (text: string) => {
    if (!searchTerm) return text

    const parts = text.split(new RegExp(`(${searchTerm})`, 'gi'))
    return parts.map((part, i) =>
      part.toLowerCase() === searchTerm?.toLowerCase() ? (
        <mark key={i} className="bg-yellow-200 dark:bg-yellow-800 rounded px-1">
          {part}
        </mark>
      ) : (
        part
      )
    )
  }

  return (
    <div
      className={cn(
        'py-0.5 px-2 whitespace-pre-wrap break-all font-mono text-xs leading-relaxed select-text',
        isHighlighted && 'bg-accent'
      )}
    >
      {highlightSearchTerm(content)}
    </div>
  )
})
