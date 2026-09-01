// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import {
  Sheet,
  SheetContent,
  SheetHeader,
  SheetTitle,
  SheetDescription,
} from '@/components/ui/sheet'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Check, Copy, ExternalLink } from 'lucide-react'
import { cn } from '@/lib/utils'

type FieldType = string | undefined

const isTimestamp = (t: FieldType) =>
  !!t && /timestamp|datetime|date|time/i.test(t)
const isUuid = (t: FieldType, name?: string) =>
  (!!t && /uuid|guid/i.test(t)) || (!!name && /(^|_)(id|uuid)$/i.test(name))
const isJson = (t: FieldType) => !!t && /json|jsonb|object|map|array/i.test(t)
const isBytes = (t: FieldType) =>
  !!t && /bytes|blob|binary|bytea/i.test(t)
const isBoolean = (t: FieldType) => !!t && /bool/i.test(t)
const isNumeric = (t: FieldType) =>
  !!t && /int|float|double|decimal|number|numeric/i.test(t)

const URL_RE = /^https?:\/\/[^\s]+$/i
const EMAIL_RE = /^[\w.+-]+@[\w-]+\.[\w.-]+$/

function formatRelative(d: Date): string {
  const diff = Date.now() - d.getTime()
  const abs = Math.abs(diff)
  const sec = Math.round(abs / 1000)
  const min = Math.round(sec / 60)
  const hr = Math.round(min / 60)
  const day = Math.round(hr / 24)
  const mo = Math.round(day / 30)
  const yr = Math.round(day / 365)
  const future = diff < 0
  let phrase: string
  if (sec < 10) phrase = 'just now'
  else if (sec < 60) phrase = `${sec}s`
  else if (min < 60) phrase = `${min}m`
  else if (hr < 24) phrase = `${hr}h`
  else if (day < 30) phrase = `${day}d`
  else if (mo < 12) phrase = `${mo}mo`
  else phrase = `${yr}y`
  if (sec < 10) return phrase
  return future ? `in ${phrase}` : `${phrase} ago`
}

function formatBytes(n: number): string {
  if (n === 0) return '0 B'
  const k = 1024
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  const i = Math.floor(Math.log(n) / Math.log(k))
  return `${Math.round((n / Math.pow(k, i)) * 100) / 100} ${units[i]}`
}

function CopyChip({ value }: { value: string }) {
  const [copied, setCopied] = useState(false)
  return (
    <button
      type="button"
      onClick={(e) => {
        e.stopPropagation()
        navigator.clipboard.writeText(value).then(() => {
          setCopied(true)
          setTimeout(() => setCopied(false), 1200)
        })
      }}
      className="opacity-0 group-hover:opacity-100 transition-opacity text-muted-foreground hover:text-foreground"
      aria-label="Copy value"
    >
      {copied ? (
        <Check className="h-3 w-3 text-green-500" />
      ) : (
        <Copy className="h-3 w-3" />
      )}
    </button>
  )
}

interface SmartCellProps {
  value: unknown
  fieldType?: string
  fieldName?: string
  /** If provided, rendering a foreign-key-shaped value can offer click-through. */
  onForeignKeyClick?: (value: string) => void
  /**
   * Expand handler for structured values.
   *
   * When supplied, the cell delegates instead of opening its own sheet. The
   * caller can then show the whole row rather than a single decontextualised
   * value — which was the complaint: you'd open a JSON blob and no longer know
   * which row it belonged to.
   */
  onExpand?: () => void
}

export function SmartCell({
  value,
  fieldType,
  fieldName,
  onForeignKeyClick,
  onExpand,
}: SmartCellProps) {
  const [jsonOpen, setJsonOpen] = useState(false)

  if (value === null || value === undefined) {
    return <span className="text-muted-foreground italic">null</span>
  }

  // Boolean
  if (isBoolean(fieldType) || typeof value === 'boolean') {
    const truthy =
      value === true || value === 'true' || value === 1 || value === 't'
    return (
      <Badge
        variant={truthy ? 'default' : 'secondary'}
        className={cn(
          'font-mono text-[10px] px-1.5 py-0',
          truthy
            ? 'bg-green-500/15 text-green-600 dark:text-green-400 hover:bg-green-500/15'
            : 'bg-muted text-muted-foreground hover:bg-muted'
        )}
      >
        {truthy ? 'true' : 'false'}
      </Badge>
    )
  }

  // JSON / object
  if (
    isJson(fieldType) ||
    (typeof value === 'object' && value !== null)
  ) {
    const str =
      typeof value === 'string'
        ? value
        : JSON.stringify(value, null, 2)
    let preview = str
    try {
      const parsed = typeof value === 'string' ? JSON.parse(str) : value
      if (Array.isArray(parsed)) {
        preview = `[${parsed.length}]`
      } else if (parsed && typeof parsed === 'object') {
        const keys = Object.keys(parsed)
        preview = `{${keys.length}}`
      }
    } catch {
      preview = str.length > 40 ? `${str.slice(0, 40)}…` : str
    }
    return (
      <>
        <button
          type="button"
          onClick={(e) => {
            e.stopPropagation()
            if (onExpand) {
              onExpand()
            } else {
              setJsonOpen(true)
            }
          }}
          className="group inline-flex items-center gap-1.5 rounded-sm border border-dashed border-border/80 bg-muted/40 px-1.5 py-0.5 text-[11px] font-mono hover:bg-muted hover:border-border transition-colors"
        >
          <span className="text-muted-foreground">{preview}</span>
          <ExternalLink className="h-3 w-3 opacity-60" />
        </button>
        <Sheet open={jsonOpen} onOpenChange={setJsonOpen}>
          <SheetContent className="w-full sm:max-w-xl overflow-y-auto">
            <SheetHeader>
              <SheetTitle className="font-mono text-sm">
                {fieldName ?? 'Value'}
              </SheetTitle>
              <SheetDescription>
                {fieldType ? `Type: ${fieldType}` : 'JSON value'}
              </SheetDescription>
            </SheetHeader>
            <div className="mt-4 flex items-center justify-end">
              <Button
                variant="outline"
                size="sm"
                onClick={() => navigator.clipboard.writeText(str)}
                className="gap-2"
              >
                <Copy className="h-3.5 w-3.5" /> Copy
              </Button>
            </div>
            <pre className="mt-2 rounded-md border bg-muted/40 p-3 text-xs font-mono whitespace-pre-wrap break-all">
              {str}
            </pre>
          </SheetContent>
        </Sheet>
      </>
    )
  }

  // Bytes
  if (isBytes(fieldType) && typeof value === 'number') {
    return <span className="font-mono text-xs">{formatBytes(value)}</span>
  }

  const str = String(value)

  // Timestamp
  if (isTimestamp(fieldType)) {
    const d = new Date(str)
    if (!isNaN(d.getTime())) {
      return (
        <span
          className="group inline-flex items-center gap-2"
          title={d.toISOString()}
        >
          <span className="font-mono text-xs">{formatRelative(d)}</span>
          <CopyChip value={d.toISOString()} />
        </span>
      )
    }
  }

  // UUID — truncated, monospace, copyable
  if (isUuid(fieldType, fieldName) && /^[0-9a-f-]{8,}$/i.test(str)) {
    const short =
      str.length > 8 ? `${str.slice(0, 4)}…${str.slice(-4)}` : str
    const fkTarget =
      onForeignKeyClick && fieldName && /_id$/i.test(fieldName)
        ? str
        : null
    return (
      <span className="group inline-flex items-center gap-1.5">
        <span
          className="font-mono text-xs cursor-help"
          title={str}
        >
          {short}
        </span>
        {fkTarget && (
          <button
            type="button"
            onClick={(e) => {
              e.stopPropagation()
              onForeignKeyClick!(fkTarget)
            }}
            className="opacity-0 group-hover:opacity-100 text-muted-foreground hover:text-foreground"
            title={`Jump to ${fieldName?.replace(/_id$/, '') ?? 'target'}`}
          >
            <ExternalLink className="h-3 w-3" />
          </button>
        )}
        <CopyChip value={str} />
      </span>
    )
  }

  // URL
  if (URL_RE.test(str)) {
    return (
      <a
        href={str}
        target="_blank"
        rel="noopener noreferrer"
        onClick={(e) => e.stopPropagation()}
        className="inline-flex items-center gap-1 text-primary hover:underline font-mono text-xs"
      >
        <span className="max-w-[24ch] truncate">{str}</span>
        <ExternalLink className="h-3 w-3 flex-shrink-0 opacity-60" />
      </a>
    )
  }

  // Email
  if (EMAIL_RE.test(str)) {
    return (
      <a
        href={`mailto:${str}`}
        onClick={(e) => e.stopPropagation()}
        className="text-primary hover:underline font-mono text-xs"
      >
        {str}
      </a>
    )
  }

  // Numeric — right-align feel via tabular-nums, but keep text-left for table layout
  if (isNumeric(fieldType) || typeof value === 'number') {
    const num = typeof value === 'number' ? value : Number(str)
    if (!isNaN(num)) {
      return (
        <span className="font-mono text-xs tabular-nums">
          {num.toLocaleString()}
        </span>
      )
    }
  }

  // Default: plain string, truncate
  return (
    <span
      className="font-mono text-xs block max-w-[48ch] truncate"
      title={str}
    >
      {str}
    </span>
  )
}
