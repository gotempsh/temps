// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  S3SourceResponse,
  SourceBackupEntry,
} from '@/api/client/types.gen'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Label } from '@/components/ui/label'
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group'
import { ResponsivePagination } from '@/components/ui/responsive-pagination'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { backupKey, restorableBackups } from '@/lib/create-from-backup'
import { formatBytesBinary } from '@/lib/server-monitoring'
import { cn } from '@/lib/utils'
import { restoreSourceBackupsQuery } from '@/pages/service-restore/restore-queries'
import { useQuery } from '@tanstack/react-query'
import { useMemo, useState } from 'react'
import { Link } from 'react-router'

const PAGE_SIZE = 5

/**
 * Picks the backup a new database starts from: a storage source, then one of
 * its backups of this engine, including backups another Temps server wrote
 * to the same bucket.
 */
export function CreationBackupSource({
  engine,
  engineName,
  sources,
  sourceId,
  onSourceChange,
  selected,
  onSelect,
  disabled = false,
}: {
  engine: string
  engineName: string
  sources: {
    isPending: boolean
    isError: boolean
    data?: Pick<S3SourceResponse, 'id' | 'name' | 'is_default'>[]
    refetch: () => unknown
  }
  sourceId: number | undefined
  onSourceChange: (sourceId: number) => void
  selected: SourceBackupEntry | null
  onSelect: (backup: SourceBackupEntry) => void
  /** Locks the choice, e.g. while the service is being created. */
  disabled?: boolean
}) {
  const [page, setPage] = useState(1)
  const backupsQuery = useQuery({
    ...restoreSourceBackupsQuery(sourceId ?? 0, true),
    enabled: sourceId !== undefined,
  })
  const backups = useMemo(
    () => restorableBackups(backupsQuery.data?.backups ?? [], engine),
    [backupsQuery.data, engine]
  )
  const totalPages = Math.max(1, Math.ceil(backups.length / PAGE_SIZE))
  const currentPage = Math.min(page, totalPages)
  const visible = backups.slice(
    (currentPage - 1) * PAGE_SIZE,
    currentPage * PAGE_SIZE
  )
  const scanErrors = backupsQuery.data?.scan_errors ?? []
  const selectedKey = selected ? backupKey(selected) : null

  const sourcesRefreshFailed = sources.isError && !!sources.data?.length
  const backupsRefreshFailed = backupsQuery.isError && !!backupsQuery.data
  const retrySources = (
    <Button
      type="button"
      variant="link"
      className="h-auto p-0"
      onClick={() => void sources.refetch()}
    >
      Retry
    </Button>
  )
  const retryBackups = (
    <Button
      type="button"
      variant="link"
      className="h-auto p-0"
      onClick={() => void backupsQuery.refetch()}
    >
      Retry
    </Button>
  )

  // A failed refresh keeps what was loaded on screen, with the error beside it.
  if (sources.isError && !sources.data?.length) {
    return (
      <Alert variant="destructive">
        <AlertTitle>Storage sources could not be loaded</AlertTitle>
        <AlertDescription>
          Backups are listed from a storage source. {retrySources}
        </AlertDescription>
      </Alert>
    )
  }
  if (sources.isPending && !sources.data) {
    return <Skeleton className="h-10 w-full" />
  }
  if (!sources.data?.length) {
    return (
      <Alert>
        <AlertTitle>No storage source is configured</AlertTitle>
        <AlertDescription>
          Add the S3-compatible bucket that holds your backups to start from one
          of them.{' '}
          <Link
            to="/backups/s3-sources/new"
            className="underline"
            target="_blank"
            rel="noreferrer"
          >
            Add a storage source
          </Link>{' '}
          <Button
            type="button"
            variant="link"
            className="h-auto p-0"
            onClick={() => void sources.refetch()}
          >
            Refresh
          </Button>
        </AlertDescription>
      </Alert>
    )
  }

  return (
    <div className="space-y-3">
      {sourcesRefreshFailed && (
        <Alert variant="destructive">
          <AlertTitle>Storage sources could not be refreshed</AlertTitle>
          <AlertDescription>
            Showing the last list loaded. {retrySources}
          </AlertDescription>
        </Alert>
      )}
      <div className="space-y-2">
        <Label htmlFor="restore-source">Storage source</Label>
        <Select
          disabled={disabled}
          value={sourceId === undefined ? undefined : String(sourceId)}
          onValueChange={(value) => {
            setPage(1)
            onSourceChange(Number(value))
          }}
        >
          <SelectTrigger id="restore-source">
            <SelectValue placeholder="Select a storage source" />
          </SelectTrigger>
          <SelectContent>
            {sources.data.map((source) => (
              <SelectItem key={source.id} value={String(source.id)}>
                {source.name}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>

      {backupsRefreshFailed && (
        <Alert variant="destructive">
          <AlertTitle>Backups could not be refreshed</AlertTitle>
          <AlertDescription>
            Showing the last list loaded. {retryBackups}
          </AlertDescription>
        </Alert>
      )}

      {sourceId === undefined ? null : backupsQuery.isError &&
        !backupsQuery.data ? (
        <Alert variant="destructive">
          <AlertTitle>Backups could not be listed</AlertTitle>
          <AlertDescription>
            Check that this server can read the bucket. {retryBackups}
          </AlertDescription>
        </Alert>
      ) : backupsQuery.isPending ? (
        <div className="space-y-2">
          {[0, 1, 2].map((row) => (
            <Skeleton key={row} className="h-14 w-full" />
          ))}
        </div>
      ) : backups.length === 0 ? (
        <Alert>
          <AlertTitle>No {engineName} backups in this source</AlertTitle>
          <AlertDescription>
            Choose another storage source, or start from an empty database.
          </AlertDescription>
        </Alert>
      ) : (
        <div className="space-y-2">
          <RadioGroup
            aria-label="Backup to restore"
            disabled={disabled}
            value={selectedKey ?? ''}
            onValueChange={(key) => {
              const backup = backups.find((entry) => backupKey(entry) === key)
              if (backup) onSelect(backup)
            }}
            className="gap-2"
          >
            {visible.map((backup) => {
              const key = backupKey(backup)
              const id = `restore-backup-${key}`
              return (
                <label
                  key={key}
                  htmlFor={id}
                  className={cn(
                    'flex w-full cursor-pointer items-start gap-3 rounded-lg border p-3 transition-colors',
                    key === selectedKey
                      ? 'border-primary bg-primary/5'
                      : 'hover:bg-accent'
                  )}
                >
                  <RadioGroupItem id={id} value={key} className="mt-0.5" />
                  <span className="min-w-0 flex-1 space-y-1">
                    <span className="flex flex-wrap items-center gap-2">
                      <span className="text-sm font-medium [overflow-wrap:anywhere]">
                        {backup.origin_service_name || backup.name}
                      </span>
                      {backup.source === 's3_scan' && (
                        <Badge variant="outline" className="text-xs">
                          Found in S3
                        </Badge>
                      )}
                    </span>
                    <span className="block text-xs text-muted-foreground">
                      {new Date(backup.created_at).toLocaleString()}
                      {backup.size_bytes ? (
                        <> · {formatBytesBinary(backup.size_bytes)}</>
                      ) : null}
                      {backup.format ? <> · {backup.format}</> : null}
                    </span>
                  </span>
                </label>
              )
            })}
          </RadioGroup>
          {totalPages > 1 && (
            <ResponsivePagination
              page={currentPage}
              pageSize={PAGE_SIZE}
              total={backups.length}
              totalPages={totalPages}
              onPageChange={setPage}
              ariaLabel="Backups"
            />
          )}
        </div>
      )}

      {scanErrors.length > 0 && (
        <Alert>
          <AlertTitle>Part of the bucket could not be read</AlertTitle>
          <AlertDescription className="[overflow-wrap:anywhere]">
            {scanErrors.join(' ')}
          </AlertDescription>
        </Alert>
      )}
    </div>
  )
}
