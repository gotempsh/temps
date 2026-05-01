import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
// Toggle UI primitive doesn't exist in this project — we render kind
// toggles as Button with `aria-pressed` and a secondary/ghost variant.
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { cn } from '@/lib/utils'
import {
  AlertOctagon,
  CircleDollarSign,
  Network,
  Workflow,
} from 'lucide-react'
import { ALL_KINDS, type EventKind } from './types'

const KIND_META: Record<
  EventKind,
  { label: string; icon: React.ComponentType<{ className?: string }> }
> = {
  request: { label: 'Requests', icon: Network },
  span: { label: 'Traces', icon: Workflow },
  error: { label: 'Errors', icon: AlertOctagon },
  revenue: { label: 'Revenue', icon: CircleDollarSign },
}

export const TIME_RANGES = [
  { value: '15m', label: 'Last 15 minutes' },
  { value: '1h', label: 'Last hour' },
  { value: '24h', label: 'Last 24 hours' },
  { value: '7d', label: 'Last 7 days' },
  { value: '30d', label: 'Last 30 days' },
] as const

export type TimeRange = (typeof TIME_RANGES)[number]['value']

export interface ObserveFilters {
  kinds: EventKind[]
  timeRange: TimeRange
  search: string
  environmentId: number | null
}

export function ObserveFilterBar({
  filters,
  onChange,
  environmentOptions,
}: {
  filters: ObserveFilters
  onChange: (next: ObserveFilters) => void
  environmentOptions?: Array<{ id: number; name: string }>
}) {
  const toggleKind = (kind: EventKind) => {
    const next = filters.kinds.includes(kind)
      ? filters.kinds.filter((k) => k !== kind)
      : [...filters.kinds, kind]
    // If user toggled the last one off, fall back to all-on so the page
    // never shows an empty list with no recourse.
    onChange({
      ...filters,
      kinds: next.length === 0 ? [...ALL_KINDS] : next,
    })
  }

  return (
    <div className="flex flex-col gap-2 border-b border-border/50 bg-background/95 p-3 sm:flex-row sm:flex-wrap sm:items-center backdrop-blur supports-[backdrop-filter]:bg-background/60">
      {/* Kind toggles */}
      <div className="flex flex-wrap items-center gap-1">
        {ALL_KINDS.map((kind) => {
          const { label, icon: Icon } = KIND_META[kind]
          const active = filters.kinds.includes(kind)
          return (
            <Button
              key={kind}
              type="button"
              variant={active ? 'secondary' : 'ghost'}
              size="sm"
              onClick={() => toggleKind(kind)}
              aria-pressed={active}
              aria-label={`Toggle ${label}`}
              className={cn(
                'h-8 gap-1.5 text-xs',
                active && 'border border-primary/30',
              )}
            >
              <Icon className="h-3.5 w-3.5" />
              <span className="hidden sm:inline">{label}</span>
            </Button>
          )
        })}
      </div>

      {/* Time range */}
      <Select
        value={filters.timeRange}
        onValueChange={(v) =>
          onChange({ ...filters, timeRange: v as TimeRange })
        }
      >
        <SelectTrigger className="w-full sm:w-[160px]">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          {TIME_RANGES.map((r) => (
            <SelectItem key={r.value} value={r.value}>
              {r.label}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>

      {/* Environment */}
      {environmentOptions && environmentOptions.length > 0 && (
        <Select
          value={filters.environmentId == null ? 'all' : String(filters.environmentId)}
          onValueChange={(v) =>
            onChange({
              ...filters,
              environmentId: v === 'all' ? null : Number(v),
            })
          }
        >
          <SelectTrigger className="w-full sm:w-[180px]">
            <SelectValue placeholder="Environment" />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="all">All environments</SelectItem>
            {environmentOptions.map((env) => (
              <SelectItem key={env.id} value={String(env.id)}>
                {env.name}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      )}

      {/* Search */}
      <Input
        type="search"
        placeholder="Search path / class / event…"
        value={filters.search}
        onChange={(e) => onChange({ ...filters, search: e.target.value })}
        className="w-full sm:w-[260px]"
      />
    </div>
  )
}
