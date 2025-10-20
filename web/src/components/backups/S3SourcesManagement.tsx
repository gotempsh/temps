'use client'

import {
  deleteS3SourceMutation,
  runBackupForSourceMutation,
  updateS3SourceMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { listS3Sources } from '@/api/client/sdk.gen'
import { S3SourceResponse } from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
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
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { cn } from '@/lib/utils'
import { useMutation, useQuery } from '@tanstack/react-query'
import {
  Database,
  MoreHorizontal,
  Pencil,
  Plus,
  RefreshCw,
  Trash2,
} from 'lucide-react'
import { useState } from 'react'
import { Link } from 'react-router-dom'
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
              (Optional, for MinIO)
            </span>
          </Label>
          <Input
            id="endpoint"
            placeholder="http://minio.example.com:9000"
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
                Enable for MinIO compatibility
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

  const deleteMutation = useMutation({
    ...deleteS3SourceMutation(),
    meta: {
      errorTitle: 'Failed to delete S3 source',
    },
    onSuccess: () => {
      refetch()
      toast.success('S3 source deleted successfully')
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

  const handleDeleteSource = (id: number) => {
    deleteMutation.mutate({
      path: { id },
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
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-lg font-semibold">S3 Sources</h2>
          <p className="text-sm text-muted-foreground">
            Configure S3 storage for backups
          </p>
        </div>
        <Button asChild>
          <Link to="/backups/s3-sources/new">
            <Plus className="mr-2 h-4 w-4" />
            Add S3 Source
          </Link>
        </Button>
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

      <Card>
        <div className="p-4">
          {isLoading ? (
            <div className="flex items-center justify-center py-6">
              <div className="animate-spin rounded-full h-8 w-8 border-b-2 border-primary"></div>
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
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Name</TableHead>
                  <TableHead>Bucket</TableHead>
                  <TableHead>Region</TableHead>
                  <TableHead className="w-[100px]">Actions</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {sources.map((source) => (
                  <TableRow key={source.id}>
                    <TableCell>
                      <Link
                        to={`/backups/s3-sources/${source.id}`}
                        className="font-medium hover:underline"
                      >
                        {source.name}
                      </Link>
                    </TableCell>
                    <TableCell>{source.bucket_name}</TableCell>
                    <TableCell>{source.region}</TableCell>
                    <TableCell>
                      <DropdownMenu>
                        <DropdownMenuTrigger asChild>
                          <Button variant="ghost" size="icon">
                            <MoreHorizontal className="h-4 w-4" />
                          </Button>
                        </DropdownMenuTrigger>
                        <DropdownMenuContent align="end">
                          <DropdownMenuItem
                            onClick={() => handleEditSource(source)}
                          >
                            <Pencil className="mr-2 h-4 w-4" />
                            Edit
                          </DropdownMenuItem>
                          <DropdownMenuItem
                            onClick={() => handleRunBackup(source.id)}
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
                          <DropdownMenuSeparator />
                          <DropdownMenuItem
                            onClick={() => handleDeleteSource(source.id)}
                            className="text-destructive"
                            disabled={deleteMutation.isPending}
                          >
                            <Trash2 className="mr-2 h-4 w-4" />
                            Delete
                          </DropdownMenuItem>
                        </DropdownMenuContent>
                      </DropdownMenu>
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          )}
        </div>
      </Card>
    </div>
  )
}
