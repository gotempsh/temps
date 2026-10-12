// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { S3SourceResponse } from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { EmptyState } from '@/components/ui/empty-state'
import {
  createBackupDestinationHref,
  scheduleServiceBackupsHref,
  type BackupDestinationState,
} from '@/lib/service-backup-setup'
import { CalendarClock, ChevronDown, HardDrive, Plus } from 'lucide-react'
import { Link } from 'react-router'

// The Backups card's setup surface on a database's page: a "Schedule
// backups" action that opens the schedule form already set to this database,
// and an empty state that onboards when no backup destination exists.

interface ScheduleBackupsActionProps {
  serviceId: number
  state: BackupDestinationState
  sources: readonly S3SourceResponse[]
}

/**
 * Header action. With one destination it opens that destination's schedule
 * form; with several it asks which one. With none it sends the operator to
 * create one instead of disappearing.
 */
export function ScheduleBackupsAction({
  serviceId,
  state,
  sources,
}: ScheduleBackupsActionProps) {
  if (state === 'loading' || state === 'unknown') return null
  if (state === 'none') {
    return (
      <Button variant="outline" size="sm" className="gap-2" asChild>
        <Link to={createBackupDestinationHref(serviceId)}>
          <Plus className="h-4 w-4" />
          <span className="hidden sm:inline">Set up backups</span>
          <span className="sm:hidden">Set up</span>
        </Link>
      </Button>
    )
  }
  if (sources.length === 1) {
    return (
      <Button variant="outline" size="sm" className="gap-2" asChild>
        <Link to={scheduleServiceBackupsHref(sources[0].id, serviceId)}>
          <CalendarClock className="h-4 w-4" />
          <span className="hidden sm:inline">Schedule backups</span>
          <span className="sm:hidden">Schedule</span>
        </Link>
      </Button>
    )
  }
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button variant="outline" size="sm" className="gap-2">
          <CalendarClock className="h-4 w-4" />
          <span className="hidden sm:inline">Schedule backups</span>
          <span className="sm:hidden">Schedule</span>
          <ChevronDown className="h-3.5 w-3.5 text-muted-foreground" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="min-w-56">
        <DropdownMenuLabel>Store the backups in</DropdownMenuLabel>
        <DropdownMenuSeparator />
        {sources.map((source) => (
          <DropdownMenuItem
            key={source.id}
            asChild
            className="min-h-12 sm:min-h-8"
          >
            <Link to={scheduleServiceBackupsHref(source.id, serviceId)}>
              <HardDrive className="mr-2 h-4 w-4" />
              <span className="truncate">{source.name}</span>
              <span className="ml-auto pl-2 text-xs text-muted-foreground">
                {source.bucket_name}
              </span>
            </Link>
          </DropdownMenuItem>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  )
}

interface ServiceBackupsEmptyProps {
  serviceId: number
  state: BackupDestinationState
  sources: readonly S3SourceResponse[]
  onTriggerBackup: () => void
}

/** The Backups card when this database has no backups yet. */
export function ServiceBackupsEmpty({
  serviceId,
  state,
  sources,
  onTriggerBackup,
}: ServiceBackupsEmptyProps) {
  if (state === 'none') {
    return (
      <EmptyState
        size="compact"
        icon={HardDrive}
        title="No backup destination configured"
        description="Backups are stored in an S3-compatible bucket, such as AWS S3, Cloudflare R2 or MinIO. Add one, then back up this database now or on a schedule, for example every night at 02:00."
        action={
          <Button size="sm" className="gap-2" asChild>
            <Link to={createBackupDestinationHref(serviceId)}>
              <Plus className="h-4 w-4" />
              Create destination
            </Link>
          </Button>
        }
      />
    )
  }
  return (
    <EmptyState
      size="compact"
      icon={HardDrive}
      title="No backups of this database yet"
      description="Take one now, or schedule recurring backups so a recent copy always exists."
      action={
        <div className="flex flex-wrap justify-center gap-2">
          <Button
            variant="outline"
            size="sm"
            className="gap-2"
            onClick={onTriggerBackup}
          >
            <HardDrive className="h-4 w-4" />
            Back up now
          </Button>
          {state === 'configured' ? (
            <ScheduleBackupsAction
              serviceId={serviceId}
              state={state}
              sources={sources}
            />
          ) : null}
        </div>
      }
    />
  )
}
