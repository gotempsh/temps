// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ServiceIcon } from '@/components/project/ServiceIcon'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { cn } from '@/lib/utils'
import type { LogServices } from './use-log-services'

/**
 * The services to choose from, where a log control needs one and the page
 * has none picked. One click chooses; nothing has to be typed.
 */
export function LogServiceList({
  services,
  onPick,
  className,
}: {
  services: LogServices
  onPick: (serviceId: number) => void
  className?: string
}) {
  if (services.isPending)
    return (
      <div className={cn('space-y-1', className)}>
        <Skeleton className="h-8 w-full" />
        <Skeleton className="h-8 w-full" />
        <Skeleton className="h-8 w-full" />
      </div>
    )
  if (services.failed)
    return (
      <div className={cn('flex items-center gap-2 text-sm', className)}>
        <span>Services could not be loaded.</span>
        <Button size="sm" variant="outline" onClick={services.retry}>
          Retry
        </Button>
      </div>
    )
  if (services.services.length === 0)
    return (
      <p className={cn('text-sm text-muted-foreground', className)}>
        No service yet. Create a project first.
      </p>
    )
  return (
    <div className={className}>
      <ul aria-label="Services" className="max-h-64 overflow-y-auto">
        {services.services.map((service) => (
          <li key={service.id}>
            <button
              type="button"
              className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm hover:bg-muted/60 focus-visible:bg-muted/60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
              onClick={() => onPick(service.id)}
            >
              <ServiceIcon service={service} size="xs" />
              <span className="min-w-0 flex-1 truncate">{service.name}</span>
            </button>
          </li>
        ))}
      </ul>
      {services.truncated && (
        <p className="px-2 pt-2 text-xs text-muted-foreground">
          Showing the first {services.services.length}. For another, type{' '}
          <code>project:</code> in the search.
        </p>
      )}
    </div>
  )
}
