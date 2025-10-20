'use client'

import { getBackupOptions } from '@/api/client/@tanstack/react-query.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { formatBytes } from '@/lib/utils'
import { useQuery } from '@tanstack/react-query'
import { format } from 'date-fns'
import { ArrowLeft, FileArchive } from 'lucide-react'
import { useEffect } from 'react'
import { Link, useParams } from 'react-router-dom'

export function BackupDetail() {
  const { id, backupId } = useParams<{ id: string; backupId: string }>()
  const { setBreadcrumbs } = useBreadcrumbs()

  const { data: backup, isLoading } = useQuery({
    ...getBackupOptions({
      path: { id: backupId! },
    }),
    enabled: !!id && !!backupId,
  })

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Backups', href: '/backups' },
      { label: 'S3 Source', href: `/backups/s3-sources/${id}` },
      { label: backup?.name || 'Backup Details' },
    ])
  }, [setBreadcrumbs, id, backup?.name])

  usePageTitle(backup?.name || 'Backup Details')

  if (isLoading) {
    return (
      <div className="flex items-center justify-center py-6">
        <div className="animate-spin rounded-full h-8 w-8 border-b-2 border-primary"></div>
      </div>
    )
  }

  if (!backup) {
    return (
      <div className="flex flex-col items-center justify-center py-6">
        <h2 className="text-lg font-semibold">Backup Not Found</h2>
        <p className="text-sm text-muted-foreground">
          The requested backup could not be found.
        </p>
        <Button asChild className="mt-4">
          <Link to={`/backups/s3-sources/${id}`}>
            <ArrowLeft className="mr-2 h-4 w-4" />
            Back to S3 Source
          </Link>
        </Button>
      </div>
    )
  }

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Button variant="ghost" size="sm" asChild>
            <Link to={`/backups/s3-sources/${id}`}>
              <ArrowLeft className="mr-2 h-4 w-4" />
              Back
            </Link>
          </Button>
        </div>
      </div>

      <div className="grid gap-6">
        <Card>
          <CardHeader>
            <CardTitle className="flex items-center gap-2">
              <FileArchive className="h-5 w-5" />
              {backup.name}
            </CardTitle>
            <CardDescription>Backup Details</CardDescription>
          </CardHeader>
          <CardContent>
            <dl className="grid gap-4">
              <div>
                <dt className="text-sm font-medium text-muted-foreground">
                  Status
                </dt>
                <dd className="text-sm">
                  <Badge
                    variant={
                      backup.state === 'completed'
                        ? 'default'
                        : backup.state === 'failed'
                          ? 'destructive'
                          : backup.state === 'running'
                            ? 'default'
                            : 'secondary'
                    }
                  >
                    {backup.state}
                  </Badge>
                </dd>
              </div>
              <div>
                <dt className="text-sm font-medium text-muted-foreground">
                  Type
                </dt>
                <dd className="text-sm">{backup.backup_type}</dd>
              </div>
              <div>
                <dt className="text-sm font-medium text-muted-foreground">
                  Created By
                </dt>
                <dd className="text-sm">{backup.created_by}</dd>
              </div>
              <div>
                <dt className="text-sm font-medium text-muted-foreground">
                  Created At
                </dt>
                <dd className="text-sm">
                  {format(new Date(backup.started_at), 'PPpp')}
                </dd>
              </div>
              {backup.completed_at && (
                <div>
                  <dt className="text-sm font-medium text-muted-foreground">
                    Finished At
                  </dt>
                  <dd className="text-sm">
                    {format(new Date(backup.completed_at), 'PPpp')}
                  </dd>
                </div>
              )}
              {(backup.metadata as { size_bytes: number })?.size_bytes && (
                <div>
                  <dt className="text-sm font-medium text-muted-foreground">
                    Size
                  </dt>
                  <dd className="text-sm">
                    {formatBytes(
                      (backup.metadata as { size_bytes: number }).size_bytes
                    )}
                  </dd>
                </div>
              )}
              {backup.file_count && (
                <div>
                  <dt className="text-sm font-medium text-muted-foreground">
                    Files
                  </dt>
                  <dd className="text-sm">{backup.file_count} files</dd>
                </div>
              )}
              <div>
                <dt className="text-sm font-medium text-muted-foreground">
                  Location
                </dt>
                <dd className="text-sm font-mono break-all">
                  {backup.s3_location}
                </dd>
              </div>
              {backup.error_message && (
                <div>
                  <dt className="text-sm font-medium text-muted-foreground">
                    Error
                  </dt>
                  <dd className="text-sm text-destructive">
                    {backup.error_message}
                  </dd>
                </div>
              )}
              {backup.tags.length > 0 && (
                <div>
                  <dt className="text-sm font-medium text-muted-foreground">
                    Tags
                  </dt>
                  <dd className="flex flex-wrap gap-2 mt-1">
                    {backup.tags.map((tag) => (
                      <Badge key={tag} variant="secondary">
                        {tag}
                      </Badge>
                    ))}
                  </dd>
                </div>
              )}
            </dl>
          </CardContent>
        </Card>
      </div>
    </div>
  )
}
