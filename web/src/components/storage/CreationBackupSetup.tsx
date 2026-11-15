// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { S3SourceResponse } from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { Link } from 'react-router'

export function CreationBackupSetup({
  query,
  destination,
  onDestinationChange,
}: {
  query: {
    isPending: boolean
    isError: boolean
    data?: S3SourceResponse[]
    refetch: () => unknown
  }
  destination: string
  onDestinationChange: (value: string) => void
}) {
  return (
    <div className="space-y-3">
      <Label htmlFor="backup-destination">Backups</Label>
      {query.isPending ? (
        <Skeleton className="h-10 w-full" />
      ) : query.isError ? (
        <Alert>
          <AlertTitle>Backup destinations unavailable</AlertTitle>
          <AlertDescription>
            Check your permissions or{' '}
            <Button
              type="button"
              variant="link"
              onClick={() => void query.refetch()}
            >
              retry
            </Button>
            . You can set up backups after creation.
          </AlertDescription>
        </Alert>
      ) : !query.data?.length ? (
        <Alert>
          <AlertTitle>No backup destination configured</AlertTitle>
          <AlertDescription>
            Add an S3-compatible destination for nightly backups.{' '}
            <Link
              to="/backups/s3-sources/new"
              className="underline"
              target="_blank"
              rel="noreferrer"
            >
              Create destination
            </Link>{' '}
            <Button
              type="button"
              variant="link"
              onClick={() => void query.refetch()}
            >
              Refresh destinations
            </Button>
          </AlertDescription>
        </Alert>
      ) : (
        <>
          <p className="text-sm text-muted-foreground">
            Choose a destination to configure a schedule after creation. No
            backups are enabled until you save that schedule.
          </p>
          <Select value={destination} onValueChange={onDestinationChange}>
            <SelectTrigger id="backup-destination">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="later">Set up backups later</SelectItem>
              {query.data.map((source) => (
                <SelectItem key={source.id} value={String(source.id)}>
                  {source.name}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </>
      )}
      {destination !== 'later' && (
        <Button
          type="button"
          variant="outline"
          onClick={() => onDestinationChange('later')}
        >
          Set up backups later
        </Button>
      )}
    </div>
  )
}
