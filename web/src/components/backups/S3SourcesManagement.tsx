// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

'use client'

import {
  deleteS3SourceMutation,
  runBackupForSourceMutation,
  updateS3SourceMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { listS3Sources } from '@/api/client/sdk.gen'
import { S3SourceResponse } from '@/api/client/types.gen'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { CreateActionButton } from '@/components/ui/create-action-button'
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { EmptyState } from '@/components/ui/empty-state'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { setDefaultS3Source, testS3SourceConnection } from '@/lib/s3-sources'
import { cn } from '@/lib/utils'
import { shouldShowS3SourceHeaderAction } from '@/lib/s3-source-presentation'
import { useMutation, useQuery } from '@tanstack/react-query'
import {
  CheckCircle2,
  ChevronRight,
  Database,
  EllipsisVertical,
  Pencil,
  PlugZap,
  Plus,
  RefreshCw,
  Star,
  Trash2,
} from 'lucide-react'
import { useState } from 'react'
import { Link } from 'react-router'
import { toast } from 'sonner'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '../ui/dropdown-menu'

interface NewS3Source {
  name: string
  bucket_name: string
  region: string
  access_key_id: string
  secret_key: string
  endpoint?: string
  force_path_style?: boolean
}

interface S3SourceFormProps {
  source?: Partial<NewS3Source>
  onSubmit: (source: NewS3Source) => void
  isSubmitting: boolean
  submitLabel: string
}

function S3SourceForm({
  source = {},
  onSubmit,
  isSubmitting,
  submitLabel,
}: S3SourceFormProps) {
  const [formData, setFormData] = useState<Partial<NewS3Source>>(source)

  const handleSubmit = () => {
    if (
      formData.name &&
      formData.bucket_name &&
      formData.region &&
      formData.access_key_id &&
      formData.secret_key
    ) {
      onSubmit(formData as NewS3Source)
    }
  }

  return (
    <>
      <div className="grid gap-4 py-4">
        <div className="grid gap-2">
          <Label htmlFor="name">Source Name</Label>
          <Input
            id="name"
            placeholder="Backup Storage"
            value={formData.name || ''}
            onChange={(e) => setFormData({ ...formData, name: e.target.value })}
          />
        </div>
        <div className="grid gap-2">
          <Label htmlFor="bucket">Bucket Name</Label>
          <Input
            id="bucket"
            placeholder="my-backups"
            value={formData.bucket_name || ''}
            onChange={(e) =>
              setFormData({ ...formData, bucket_name: e.target.value })
            }
          />
        </div>
        <div className="grid gap-2">
          <Label htmlFor="region">Region</Label>
          <Input
            id="region"
            placeholder="us-east-1"
            value={formData.region || ''}
            onChange={(e) =>
              setFormData({ ...formData, region: e.target.value })
            }
          />
        </div>
        <div className="grid gap-2">
          <Label
            htmlFor="endpoint"
            className="flex items-baseline justify-between"
          >
            <span>Endpoint URL</span>
            <span className="text-xs text-muted-foreground">
              (Optional, for RustFS/MinIO)
            </span>
          </Label>
          <Input
            id="endpoint"
            placeholder="http://rustfs.example.com:9000"
            value={formData.endpoint || ''}
            onChange={(e) =>
              setFormData({ ...formData, endpoint: e.target.value })
            }
          />
        </div>
        <div className="grid gap-2">
          <Label
            htmlFor="forcePathStyle"
            className="flex items-center space-x-2"
          >
            <Input
              id="forcePathStyle"
              type="checkbox"
              className="h-4 w-4"
              checked={formData.force_path_style || false}
              onChange={(e) =>
                setFormData({ ...formData, force_path_style: e.target.checked })
              }
            />
            <div>
              <span>Force Path Style</span>
              <p className="text-xs text-muted-foreground">
                Enable for RustFS/MinIO compatibility
              </p>
            </div>
          </Label>
        </div>
        <div className="grid gap-2">
          <Label htmlFor="accessKeyId">Access Key ID</Label>
          <Input
            id="accessKeyId"
            type="password"
            placeholder="AKIAXXXXXXXXXXXXXXXX"
            value={formData.access_key_id || ''}
            onChange={(e) =>
              setFormData({ ...formData, access_key_id: e.target.value })
            }
          />
        </div>
        <div className="grid gap-2">
          <Label htmlFor="secretKey">Secret Key</Label>
          <Input
            id="secretKey"
            type="password"
            placeholder="Enter your AWS secret key"
            value={formData.secret_key || ''}
            onChange={(e) =>
              setFormData({ ...formData, secret_key: e.target.value })
            }
          />
        </div>
      </div>
      <DialogFooter>
        <Button onClick={handleSubmit} disabled={isSubmitting}>
          {isSubmitting ? 'Saving...' : submitLabel}
        </Button>
      </DialogFooter>
    </>
  )
}

export function S3SourcesManagement() {
  const [isEditDialogOpen, setIsEditDialogOpen] = useState(false)
  const [selectedSource, setSelectedSource] = useState<
    (Partial<NewS3Source> & { id?: number }) | null
  >(null)
  const [pendingDefault, setPendingDefault] = useState<S3SourceResponse | null>(
    null
  )
  const [sourceToDelete, setSourceToDelete] = useState<S3SourceResponse | null>(
    null
  )

  const {
    data: sources = [],
    refetch,
    isLoading,
  } = useQuery({
    queryKey: ['s3Sources'],
    queryFn: async () => {
      const { data } = await listS3Sources()
      return data
    },
  })

  const setDefaultMutation = useMutation({
    mutationFn: (id: number) => setDefaultS3Source(id),
    meta: { errorTitle: 'Failed to set default S3 source' },
    onSuccess: () => {
      toast.success('Default S3 source updated')
      setPendingDefault(null)
      refetch()
    },
    onError: (e: Error) => toast.error(e.message),
  })

  const testConnectionMutation = useMutation({
    mutationFn: (id: number) => testS3SourceConnection(id),
    meta: { errorTitle: 'Failed to test S3 connection' },
    onSuccess: (result) => {
      if (result.ok) {
        toast.success(result.message || 'Connection successful')
      } else {
        toast.error(result.message || 'Connection failed')
      }
    },
    onError: (e: Error) => toast.error(e.message),
  })

  const deleteMutation = useMutation({
    ...deleteS3SourceMutation(),
    meta: {
      errorTitle: 'Failed to delete S3 source',
    },
    onSuccess: () => {
      refetch()
      toast.success('S3 source deleted successfully')
    },
    onSettled: () => {
      setSourceToDelete(null)
    },
  })

  const runBackupMutation = useMutation({
    ...runBackupForSourceMutation(),
    meta: {
      errorTitle: 'Failed to start backup',
    },
    onSuccess: () => {
      toast.success('Backup started successfully')
    },
  })

  const updateMutation = useMutation({
    ...updateS3SourceMutation(),
    meta: {
      errorTitle: 'Failed to update S3 source',
    },
    onSuccess: () => {
      refetch()
      setSelectedSource(null)
      setIsEditDialogOpen(false)
      toast.success('S3 source updated successfully')
    },
  })

  const confirmDeleteSource = () => {
    if (!sourceToDelete) return
    deleteMutation.mutate({
      path: { id: sourceToDelete.id },
    })
  }

  const handleRunBackup = (id: number) => {
    toast.promise(
      runBackupMutation.mutateAsync({
        path: { id },
        body: {
          backup_type: 'manual',
        },
      }),
      {
        loading: 'Starting backup...',
      }
    )
  }

  const handleEditSource = (source: S3SourceResponse) => {
    setSelectedSource({
      id: source.id,
      name: source.name,
      bucket_name: source.bucket_name,
      region: source.region,
      access_key_id: source.access_key_id,
      secret_key: '',
      endpoint: source.endpoint || undefined,
      force_path_style: source.force_path_style || undefined,
    })
    setIsEditDialogOpen(true)
  }

  const handleUpdateSource = (updatedSource: NewS3Source) => {
    if (selectedSource && 'id' in selectedSource && selectedSource.id) {
      updateMutation.mutate({
        path: { id: selectedSource.id },
        body: {
          ...updatedSource,
          bucket_path: '/',
        },
      })
    }
  }

  return (
    <div className="space-y-6">
      <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        <div>
          <h2 className="text-lg font-semibold">S3 sources</h2>
          <p className="text-sm text-muted-foreground">
            Configure S3 storage for backups
          </p>
        </div>
        {shouldShowS3SourceHeaderAction(isLoading, sources.length) ? (
          <CreateActionButton
            to="/backups/s3-sources/new"
            label="Add S3 Source"
            className="w-full sm:w-auto"
          />
        ) : null}
      </div>

      <Dialog open={isEditDialogOpen} onOpenChange={setIsEditDialogOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Edit S3 Source</DialogTitle>
          </DialogHeader>
          <S3SourceForm
            source={selectedSource || {}}
            onSubmit={handleUpdateSource}
            isSubmitting={updateMutation.isPending}
            submitLabel="Save Changes"
          />
        </DialogContent>
      </Dialog>

      <Dialog
        open={pendingDefault !== null}
        onOpenChange={(open) => !open && setPendingDefault(null)}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Make this the default S3 source?</DialogTitle>
          </DialogHeader>
          {pendingDefault ? (
            <div className="space-y-3 text-sm">
              <p>
                All future backups and WAL archives for services using the
                default source will go to{' '}
                <span className="font-medium">{pendingDefault.name}</span> (
                <code>{pendingDefault.bucket_name}</code>).
              </p>
              <p className="text-muted-foreground">
                Existing backup schedules keep their explicitly-configured
                source. External services (like Postgres WAL archiving) that
                track the default source will begin writing to the new location
                on their next backup.
              </p>
            </div>
          ) : null}
          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => setPendingDefault(null)}
              disabled={setDefaultMutation.isPending}
            >
              Cancel
            </Button>
            <Button
              onClick={() =>
                pendingDefault && setDefaultMutation.mutate(pendingDefault.id)
              }
              disabled={setDefaultMutation.isPending}
            >
              {setDefaultMutation.isPending ? 'Updating...' : 'Set as default'}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {isLoading ? (
        <div className="divide-y rounded-lg border">
          {Array.from({ length: 3 }).map((_, i) => (
            <div
              key={i}
              className="flex items-center gap-4 px-4 py-3 animate-pulse"
            >
              <div className="size-9 shrink-0 rounded-md bg-muted" />
              <div className="flex-1 min-w-0 space-y-1.5">
                <div className="h-4 w-48 bg-muted rounded" />
                <div className="h-3 w-64 bg-muted rounded" />
              </div>
            </div>
          ))}
        </div>
      ) : sources.length === 0 ? (
        <EmptyState
          icon={Database}
          title="No S3 sources configured"
          description="Add an S3 source to store your backups"
          action={
            <Button asChild>
              <Link to="/backups/s3-sources/new">
                <Plus className="mr-2 h-4 w-4" />
                Add S3 Source
              </Link>
            </Button>
          }
        />
      ) : (
        <div className="overflow-hidden rounded-lg border">
          <ul role="list" className="divide-y">
            {sources.map((source) => {
              const isDefault =
                (source as S3SourceResponse & { is_default?: boolean })
                  .is_default === true
              const isTestingThis =
                testConnectionMutation.isPending &&
                testConnectionMutation.variables === source.id
              return (
                <li
                  key={source.id}
                  className="flex items-center gap-4 px-4 py-3 hover:bg-muted/40 transition-colors"
                >
                  {/* Wrap icon + text in a Link so ⌘-click, middle-click, and
                      right-click → "Open in new tab" all work natively. The
                      row retains its hover highlight for visual continuity. */}
                  <Link
                    to={`/backups/s3-sources/${source.id}`}
                    className="flex min-w-0 flex-1 items-center gap-4"
                  >
                    <div className="flex size-9 shrink-0 items-center justify-center rounded-md bg-muted">
                      <Database className="size-4 text-muted-foreground" />
                    </div>
                    <div className="flex min-w-0 flex-1 items-center gap-3">
                      <div className="min-w-0 flex-1">
                        <div className="flex items-center gap-2 flex-wrap">
                          <p className="truncate text-sm font-medium">
                            {source.name}
                          </p>
                          <Badge
                            variant="secondary"
                            className="font-mono text-xs"
                          >
                            {source.bucket_name}
                          </Badge>
                          {isDefault && (
                            <Badge
                              variant="outline"
                              className="gap-1 border-amber-400/40 text-amber-600 dark:text-amber-300"
                            >
                              <Star className="size-3 fill-current" />
                              Default
                            </Badge>
                          )}
                        </div>
                        <p className="mt-0.5 truncate text-xs text-muted-foreground">
                          {source.region}
                          {source.endpoint ? ` · ${source.endpoint}` : ''}
                        </p>
                      </div>
                    </div>
                    <ChevronRight className="size-4 shrink-0 text-muted-foreground/50" />
                  </Link>
                  <div>
                    <DropdownMenu>
                      <DropdownMenuTrigger asChild>
                        <Button variant="ghost" size="icon" className="h-8 w-8">
                          <EllipsisVertical className="h-4 w-4" />
                        </Button>
                      </DropdownMenuTrigger>
                      <DropdownMenuContent align="end">
                        <DropdownMenuItem
                          onSelect={(e) => {
                            e.preventDefault()
                            handleEditSource(source)
                          }}
                        >
                          <Pencil className="mr-2 h-4 w-4" />
                          Edit
                        </DropdownMenuItem>
                        <DropdownMenuItem
                          onSelect={(e) => {
                            e.preventDefault()
                            testConnectionMutation.mutate(source.id)
                          }}
                          disabled={testConnectionMutation.isPending}
                        >
                          {isTestingThis ? (
                            <RefreshCw className="mr-2 h-4 w-4 animate-spin" />
                          ) : (
                            <PlugZap className="mr-2 h-4 w-4" />
                          )}
                          {isTestingThis ? 'Testing...' : 'Test connection'}
                        </DropdownMenuItem>
                        <DropdownMenuItem
                          onSelect={(e) => {
                            e.preventDefault()
                            handleRunBackup(source.id)
                          }}
                          disabled={runBackupMutation.isPending}
                        >
                          <RefreshCw
                            className={cn('mr-2 h-4 w-4', {
                              'animate-spin': runBackupMutation.isPending,
                            })}
                          />
                          {runBackupMutation.isPending
                            ? 'Starting...'
                            : 'Run Now'}
                        </DropdownMenuItem>
                        {!isDefault && (
                          <DropdownMenuItem
                            onSelect={(e) => {
                              e.preventDefault()
                              setPendingDefault(source)
                            }}
                          >
                            <CheckCircle2 className="mr-2 h-4 w-4" />
                            Set as default
                          </DropdownMenuItem>
                        )}
                        <DropdownMenuSeparator />
                        <DropdownMenuItem
                          onSelect={(e) => {
                            e.preventDefault()
                            setSourceToDelete(source)
                          }}
                          className="text-destructive"
                          disabled={deleteMutation.isPending}
                        >
                          <Trash2 className="mr-2 h-4 w-4" />
                          Delete
                        </DropdownMenuItem>
                      </DropdownMenuContent>
                    </DropdownMenu>
                  </div>
                </li>
              )
            })}
          </ul>
        </div>
      )}

      <AlertDialog
        open={sourceToDelete !== null}
        onOpenChange={(open) => {
          if (!open && !deleteMutation.isPending) {
            setSourceToDelete(null)
          }
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete S3 source?</AlertDialogTitle>
            <AlertDialogDescription>
              This will remove
              {sourceToDelete ? (
                <>
                  {' '}
                  <span className="font-medium text-foreground">
                    {sourceToDelete.name}
                  </span>{' '}
                  (<code>{sourceToDelete.bucket_name}</code>)
                </>
              ) : null}{' '}
              from Temps. Backup schedules pointing at this source will fail on
              their next run, and services that rely on it for WAL archiving
              will stop shipping new data until reconfigured. Objects already in
              the bucket will not be deleted. This action cannot be undone.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={deleteMutation.isPending}>
              Cancel
            </AlertDialogCancel>
            <AlertDialogAction
              onClick={(e) => {
                e.preventDefault()
                confirmDeleteSource()
              }}
              disabled={deleteMutation.isPending}
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
            >
              {deleteMutation.isPending ? 'Deleting...' : 'Delete source'}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  )
}
