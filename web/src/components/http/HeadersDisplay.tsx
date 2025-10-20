import { useMemo } from 'react'

interface HeadersDisplayProps {
  headers: string | Record<string, unknown> | null | undefined
  emptyMessage?: string
}

export function HeadersDisplay({
  headers,
  emptyMessage = 'Failed to parse headers',
}: HeadersDisplayProps) {
  const parsedHeaders = useMemo(() => {
    if (!headers) return null

    try {
      if (typeof headers === 'string') {
        return JSON.parse(headers)
      }
      return headers
    } catch {
      return null
    }
  }, [headers])

  if (!parsedHeaders) {
    return <p className="text-sm text-muted-foreground">{emptyMessage}</p>
  }

  const headerEntries = Object.entries(parsedHeaders)

  if (headerEntries.length === 0) {
    return <p className="text-sm text-muted-foreground">No headers available</p>
  }

  return (
    <div className="space-y-3">
      {headerEntries.map(([key, value]) => (
        <div key={key} className="border-b pb-2 last:border-0">
          <div className="flex flex-col space-y-1">
            <span className="text-sm font-medium">{key}</span>
            <span className="text-sm text-muted-foreground font-mono break-all">
              {Array.isArray(value) ? value.join(', ') : String(value)}
            </span>
          </div>
        </div>
      ))}
    </div>
  )
}
