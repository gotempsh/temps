// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

'use client'

/**
 * Picker for selecting external services to back up. Used by:
 *   - CreateBackupSchedule (initial selection)
 *   - ScheduleDetail (attach more)
 *
 * Behaviour:
 *   - Shows every external service the host knows about, ordered by name.
 *   - A "Select all" master toggle that's checked by default for create
 *     flows (matches the user expectation: "back up all my DBs").
 *   - Caller controls the `value` (selected service ids) so this stays
 *     dumb and reusable.
 *
 * The `excludeIds` prop hides already-attached services on the detail page.
 */

import { Checkbox } from '@/components/ui/checkbox'
import { Skeleton } from '@/components/ui/skeleton'
import { listServicesOptions } from '@/api/client/@tanstack/react-query.gen'
import { useQuery } from '@tanstack/react-query'
import { Database, HardDrive } from 'lucide-react'

interface Props {
  /** Selected service ids. */
  value: number[]
  /** Called with the new selection on every change. */
  onChange: (next: number[]) => void
  /** Hide these service ids (e.g. already attached). */
  excludeIds?: number[]
  /** Disable interaction (during a mutation). */
  disabled?: boolean
}

export function ScheduleServicesSelector({
  value,
  onChange,
  excludeIds = [],
  disabled = false,
}: Props) {
  const { data: services, isPending } = useQuery({
    ...listServicesOptions({ query: { page_size: 100 } }),
  })

  const visible = (services ?? []).filter(
    (s) => !excludeIds.includes(s.id),
  )
  const allSelected =
    visible.length > 0 && visible.every((s) => value.includes(s.id))
  const someSelected = visible.some((s) => value.includes(s.id))

  function toggleAll() {
    if (allSelected) {
      onChange(value.filter((id) => !visible.some((s) => s.id === id)))
    } else {
      const next = new Set(value)
      visible.forEach((s) => next.add(s.id))
      onChange(Array.from(next))
    }
  }

  function toggleOne(id: number) {
    if (value.includes(id)) {
      onChange(value.filter((v) => v !== id))
    } else {
      onChange([...value, id])
    }
  }

  if (isPending) {
    return (
      <div className="space-y-2">
        <Skeleton className="h-9 w-full" />
        <Skeleton className="h-9 w-full" />
        <Skeleton className="h-9 w-full" />
      </div>
    )
  }

  if (visible.length === 0) {
    return (
      <div className="rounded-md border border-dashed p-4 text-sm text-muted-foreground">
        No external services found. Add a Postgres, Redis, MongoDB, or RustFS
        service first and they'll appear here.
      </div>
    )
  }

  return (
    <div className="space-y-1">
      <label className="flex items-center gap-3 rounded-md px-2 py-2 text-sm font-medium hover:bg-muted/40 cursor-pointer">
        <Checkbox
          checked={allSelected ? true : someSelected ? 'indeterminate' : false}
          onCheckedChange={toggleAll}
          disabled={disabled}
          aria-label="Select all services"
        />
        <span>Select all ({visible.length})</span>
      </label>
      <div className="border-t" />
      <div className="max-h-72 overflow-y-auto">
        {visible.map((svc) => {
          const checked = value.includes(svc.id)
          const Icon = svc.service_type === 's3' ? HardDrive : Database
          return (
            <label
              key={svc.id}
              className="flex items-center gap-3 rounded-md px-2 py-2 text-sm hover:bg-muted/40 cursor-pointer"
            >
              <Checkbox
                checked={checked}
                onCheckedChange={() => toggleOne(svc.id)}
                disabled={disabled}
                aria-label={`Select ${svc.name}`}
              />
              <Icon className="h-4 w-4 text-muted-foreground" aria-hidden />
              <span className="flex-1 truncate">{svc.name}</span>
              <span className="rounded bg-muted px-1.5 py-0.5 text-xs text-muted-foreground">
                {svc.service_type}
              </span>
            </label>
          )
        })}
      </div>
    </div>
  )
}
