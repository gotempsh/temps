'use client'

import {
  getS3SourceOptions,
  listSourceBackupsOptions,
} from '@/api/client/@tanstack/react-query.gen'
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
import { useQuery } from '@tanstack/react-query'
import { format } from 'date-fns'
import { ArrowLeft, Database, DatabaseBackup } from 'lucide-react'
import { useEffect, useMemo } from 'react'
import { Link, useParams } from 'react-router-dom'

export function S3SourceDetail() {
  const { id } = useParams<{ id: string }>()
  const { setBreadcrumbs } = useBreadcrumbs()

  const {
    data: source,
    isLoading: isLoadingSource,
    refetch: refetchSource,
  } = useQuery({
    ...getS3SourceOptions({
      path: { id: parseInt(id!) },
    }),
    enabled: !!id,
  })

  const {
    data: backups,
    isLoading: isLoadingBackups,
    refetch: refetchBackups,
  } = useQuery({
    ...listSourceBackupsOptions({
      path: { id: parseInt(id!) },
    }),
    enabled: !!id,
  })

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Backups', href: '/backups' },
      { label: source?.name || 'S3 Source Details' },
    ])
  }, [setBreadcrumbs, source?.name])

  usePageTitle(source?.name || 'S3 Source Details')

  const isLoading = isLoadingSource || isLoadingBackups
  const sortedBackups = useMemo(
    () =>
      [...(backups?.backups || [])].sort((a, b) => {
        return (
          new Date(b.created_at).getTime() - new Date(a.created_at).getTime()
        )
      }),
    [backups]
  )
  if (isLoading) {
    return (
      <div className="flex items-center justify-center py-6">
        <div className="animate-spin rounded-full h-8 w-8 border-b-2 border-primary"></div>
      </div>
    )
  }

  if (!source) {
    return (
      <div className="flex flex-col items-center justify-center py-6">
        <h2 className="text-lg font-semibold">S3 Source Not Found</h2>
        <p className="text-sm text-muted-foreground">
          The requested S3 source could not be found.
        </p>
        <Button asChild className="mt-4">
          <Link to="/backups">
            <ArrowLeft className="mr-2 h-4 w-4" />
            Back to Backups
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
            <Link to="/backups">
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
              <Database className="h-5 w-5" />
              {source.name}
            </CardTitle>
            <CardDescription>S3 Storage Configuration</CardDescription>
          </CardHeader>
          <CardContent>
            <dl className="grid gap-4">
              <div>
                <dt className="text-sm font-medium text-muted-foreground">
                  Bucket Name
                </dt>
                <dd className="text-sm">{source.bucket_name}</dd>
              </div>
              <div>
                <dt className="text-sm font-medium text-muted-foreground">
                  Region
                </dt>
                <dd className="text-sm">{source.region}</dd>
              </div>
              {source.endpoint && (
                <div>
                  <dt className="text-sm font-medium text-muted-foreground">
                    Endpoint URL
                  </dt>
                  <dd className="text-sm">{source.endpoint}</dd>
                </div>
              )}
              <div>
                <dt className="text-sm font-medium text-muted-foreground">
                  Force Path Style
                </dt>
                <dd className="text-sm">
                  <Badge
                    variant={source.force_path_style ? 'default' : 'secondary'}
                  >
                    {source.force_path_style ? 'Enabled' : 'Disabled'}
                  </Badge>
                </dd>
              </div>
              <div>
                <dt className="text-sm font-medium text-muted-foreground">
                  Access Key ID
                </dt>
                <dd className="text-sm font-mono">•••••••••••••••••••••</dd>
              </div>
              <div>
                <dt className="text-sm font-medium text-muted-foreground">
                  Secret Key
                </dt>
                <dd className="text-sm font-mono">•••••••••••••••••••••</dd>
              </div>
            </dl>
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>Recent Backups</CardTitle>
            <CardDescription>
              List of recent backups using this S3 source
            </CardDescription>
          </CardHeader>
          <CardContent>
            {sortedBackups.length === 0 ? (
              <p className="text-sm text-muted-foreground">
                No backups found for this S3 source.
              </p>
            ) : (
              <div className="space-y-4">
                {sortedBackups?.map((backup) => (
                  <Link
                    key={backup.backup_id}
                    to={`/backups/s3-sources/${id}/backups/${backup.backup_id}`}
                    className="block"
                  >
                    <div className="flex items-center justify-between p-4 border rounded-lg hover:bg-muted/50 transition-colors">
                      <div className="flex items-center gap-4">
                        <DatabaseBackup className="h-4 w-4" />
                        <div className="text-sm">
                          {format(new Date(backup.created_at), 'PPP p')}
                        </div>
                      </div>
                    </div>
                  </Link>
                ))}
              </div>
            )}
          </CardContent>
        </Card>
      </div>
    </div>
  )
}
