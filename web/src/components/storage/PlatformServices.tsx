// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { useAuth } from '@/contexts/AuthContext'
import { ReadFailure } from '@/components/ui/read-failure'
import {
  kvStatusOptions,
  kvEnableMutation,
  kvDisableMutation,
  kvUpdateMutation,
  blobStatusOptions,
  blobEnableMutation,
  blobDisableMutation,
  blobUpdateMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Database,
  HardDrive,
  CheckCircle2,
  XCircle,
  Loader2,
  Info,
  Power,
  PowerOff,
  Settings,
} from 'lucide-react'
import { Skeleton } from '@/components/ui/skeleton'
import { toast } from 'sonner'
import { DEFAULT_RUSTFS_IMAGE } from '@/lib/service-images'

type ServiceType = 'kv' | 'blob'

interface EditDockerImageDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  serviceName: string
  dockerImage: string
  onDockerImageChange: (image: string) => void
  onSave: (newImage: string) => void
  isPending: boolean
  available: boolean
}

function EditDockerImageDialog({
  open,
  onOpenChange,
  serviceName,
  dockerImage,
  onDockerImageChange,
  onSave,
  isPending,
  available,
}: EditDockerImageDialogProps) {
  const handleSave = () => {
    if (available && dockerImage.trim()) {
      onSave(dockerImage.trim())
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Update {serviceName} Configuration</DialogTitle>
          <DialogDescription>
            Change the Docker image for the {serviceName} service. This will
            restart the service with the new image.
          </DialogDescription>
        </DialogHeader>
        <div className="space-y-4 py-4">
          <div className="space-y-2">
            <Label htmlFor="docker-image">Docker Image</Label>
            <Input
              id="docker-image"
              value={dockerImage}
              onChange={(e) => onDockerImageChange(e.target.value)}
              placeholder={
                serviceName === 'KV Store'
                  ? 'redis:8-alpine'
                  : DEFAULT_RUSTFS_IMAGE
              }
            />
            <p className="text-xs text-muted-foreground">
              {serviceName === 'KV Store'
                ? 'Examples: redis:8-alpine, valkey/valkey:8-alpine'
                : `Examples: ${DEFAULT_RUSTFS_IMAGE}, rustfs/rustfs:latest`}
            </p>
          </div>
        </div>
        {!available && (
          <p role="status" className="text-sm text-muted-foreground">
            Configuration changes require administrator access and a successful
            status check. Close this dialog and retry the status read.
          </p>
        )}
        <DialogFooter>
          <Button
            variant="outline"
            onClick={() => onOpenChange(false)}
            disabled={isPending}
          >
            Cancel
          </Button>
          <Button
            onClick={handleSave}
            disabled={isPending || !available || !dockerImage.trim()}
          >
            {isPending ? (
              <>
                <Loader2 className="h-4 w-4 animate-spin mr-2" />
                Updating...
              </>
            ) : (
              'Update'
            )}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

export function PlatformServices() {
  const queryClient = useQueryClient()
  const { user } = useAuth()
  const canManage = user?.role === 'admin' || user?.role === 'platform_admin'
  const [editDialogOpen, setEditDialogOpen] = useState(false)
  const [editingService, setEditingService] = useState<ServiceType | null>(null)
  const [editDockerImage, setEditDockerImage] = useState('')

  // Fetch KV status
  const kvQuery = useQuery({
    ...kvStatusOptions(),
    refetchInterval: 10000,
    retry: false,
  })

  // Fetch Blob status
  const blobQuery = useQuery({
    ...blobStatusOptions(),
    refetchInterval: 10000,
    retry: false,
  })

  // KV mutations
  const kvEnableMut = useMutation({
    ...kvEnableMutation(),
    onSuccess: () => {
      toast.success('KV Store enabled successfully')
      queryClient.invalidateQueries({ queryKey: kvStatusOptions().queryKey })
    },
    onError: (error: Error) => {
      toast.error(error.message || 'Failed to enable KV Store')
    },
  })

  const kvDisableMut = useMutation({
    ...kvDisableMutation(),
    onSuccess: () => {
      toast.success('KV Store disabled successfully')
      queryClient.invalidateQueries({ queryKey: kvStatusOptions().queryKey })
    },
    onError: (error: Error) => {
      toast.error(error.message || 'Failed to disable KV Store')
    },
  })

  const kvUpdateMut = useMutation({
    ...kvUpdateMutation(),
    onSuccess: () => {
      toast.success('KV Store updated successfully')
      queryClient.invalidateQueries({ queryKey: kvStatusOptions().queryKey })
      setEditDialogOpen(false)
      setEditingService(null)
    },
    onError: (error: Error) => {
      toast.error(error.message || 'Failed to update KV Store')
    },
  })

  // Blob mutations
  const blobEnableMut = useMutation({
    ...blobEnableMutation(),
    onSuccess: () => {
      toast.success('Blob Storage enabled successfully')
      queryClient.invalidateQueries({ queryKey: blobStatusOptions().queryKey })
    },
    onError: (error: Error) => {
      toast.error(error.message || 'Failed to enable Blob Storage')
    },
  })

  const blobDisableMut = useMutation({
    ...blobDisableMutation(),
    onSuccess: () => {
      toast.success('Blob Storage disabled successfully')
      queryClient.invalidateQueries({ queryKey: blobStatusOptions().queryKey })
    },
    onError: (error: Error) => {
      toast.error(error.message || 'Failed to disable Blob Storage')
    },
  })

  const blobUpdateMut = useMutation({
    ...blobUpdateMutation(),
    onSuccess: () => {
      toast.success('Blob Storage updated successfully')
      queryClient.invalidateQueries({ queryKey: blobStatusOptions().queryKey })
      setEditDialogOpen(false)
      setEditingService(null)
    },
    onError: (error: Error) => {
      toast.error(error.message || 'Failed to update Blob Storage')
    },
  })

  const kvStatus = kvQuery.data
  const blobStatus = blobQuery.data

  const handleEditKv = () => {
    setEditDockerImage(kvStatus?.docker_image || 'redis:8-alpine')
    setEditingService('kv')
    setEditDialogOpen(true)
  }

  const handleEditBlob = () => {
    setEditDockerImage(blobStatus?.docker_image || DEFAULT_RUSTFS_IMAGE)
    setEditingService('blob')
    setEditDialogOpen(true)
  }

  const handleSaveDockerImage = (newImage: string) => {
    if (!canManage) return
    if (editingService === 'kv' && kvQuery.isSuccess) {
      kvUpdateMut.mutate({ body: { docker_image: newImage } })
    } else if (editingService === 'blob' && blobQuery.isSuccess) {
      blobUpdateMut.mutate({ body: { docker_image: newImage } })
    }
  }

  const currentServiceName =
    editingService === 'kv' ? 'KV Store' : 'Blob Storage'

  return (
    <div className="space-y-6">
      <Alert>
        <Info className="h-4 w-4" />
        <AlertTitle>Platform Services</AlertTitle>
        <AlertDescription>
          These services are shared across all projects. Each project’s data is
          isolated by namespace. Enable a service to make it available for all
          projects.
        </AlertDescription>
      </Alert>

      <div className="grid gap-6 md:grid-cols-2">
        {/* KV Store Service */}
        <ServiceCard
          name="KV Store"
          description="Redis-backed key-value storage for caching, sessions, and real-time data"
          icon={Database}
          loading={kvQuery.isPending}
          error={kvQuery.isError ? kvQuery.error : undefined}
          lastChecked={kvQuery.dataUpdatedAt}
          onRetry={() => kvQuery.refetch()}
          retrying={kvQuery.isFetching}
          canManage={canManage && kvQuery.isSuccess}
          enabled={kvStatus?.enabled}
          healthy={kvStatus?.healthy ?? false}
          version={kvStatus?.version}
          dockerImage={kvStatus?.docker_image}
          features={[
            'Fast in-memory storage',
            'TTL support for automatic expiration',
            'Atomic operations (INCR, DECR)',
            'Pattern-based key matching',
          ]}
          onEnable={() => kvEnableMut.mutate({ body: {} })}
          onDisable={() => kvDisableMut.mutate({})}
          onEdit={handleEditKv}
          isEnabling={kvEnableMut.isPending}
          isDisabling={kvDisableMut.isPending}
        />

        {/* Blob Storage Service */}
        <ServiceCard
          name="Blob Storage"
          description="S3-compatible object storage for files, images, and large data"
          icon={HardDrive}
          loading={blobQuery.isPending}
          error={blobQuery.isError ? blobQuery.error : undefined}
          lastChecked={blobQuery.dataUpdatedAt}
          onRetry={() => blobQuery.refetch()}
          retrying={blobQuery.isFetching}
          canManage={canManage && blobQuery.isSuccess}
          enabled={blobStatus?.enabled}
          healthy={blobStatus?.healthy ?? false}
          version={blobStatus?.version}
          dockerImage={blobStatus?.docker_image}
          features={[
            'S3-compatible API',
            'Automatic content type detection',
            'Streaming uploads/downloads',
            'Prefix-based listing',
          ]}
          onEnable={() => blobEnableMut.mutate({ body: {} })}
          onDisable={() => blobDisableMut.mutate({})}
          onEdit={handleEditBlob}
          isEnabling={blobEnableMut.isPending}
          isDisabling={blobDisableMut.isPending}
        />
      </div>

      {/* Edit Docker Image Dialog */}
      <EditDockerImageDialog
        open={editDialogOpen}
        onOpenChange={(open) => {
          setEditDialogOpen(open)
          if (!open) setEditingService(null)
        }}
        serviceName={currentServiceName}
        dockerImage={editDockerImage}
        onDockerImageChange={setEditDockerImage}
        onSave={handleSaveDockerImage}
        isPending={kvUpdateMut.isPending || blobUpdateMut.isPending}
        available={
          canManage &&
          (editingService === 'kv' ? kvQuery.isSuccess : blobQuery.isSuccess)
        }
      />
    </div>
  )
}

interface ServiceCardProps {
  name: string
  description: string
  icon: React.ComponentType<{ className?: string }>
  loading: boolean
  error?: unknown
  lastChecked: number
  onRetry: () => void
  retrying: boolean
  canManage: boolean
  enabled?: boolean
  healthy: boolean
  version?: string | null
  dockerImage?: string | null
  features: string[]
  onEnable: () => void
  onDisable: () => void
  onEdit: () => void
  isEnabling: boolean
  isDisabling: boolean
}

function ServiceCard({
  name,
  description,
  icon: Icon,
  loading,
  error,
  lastChecked,
  onRetry,
  retrying,
  canManage,
  enabled,
  healthy,
  version,
  dockerImage,
  features,
  onEnable,
  onDisable,
  onEdit,
  isEnabling,
  isDisabling,
}: ServiceCardProps) {
  const isPending = isEnabling || isDisabling || !canManage
  const unknown = !!error || enabled === undefined
  if (loading) return <ServiceCardSkeleton />

  return (
    <Card className="flex flex-col shadow-none">
      <CardHeader>
        <div className="flex items-start justify-between">
          <div className="flex items-center gap-3">
            <div className="p-2 rounded-lg bg-primary/10">
              <Icon className="h-6 w-6 text-primary" />
            </div>
            <div>
              <CardTitle className="text-lg">{name}</CardTitle>
              <Badge
                variant={
                  unknown
                    ? 'secondary'
                    : enabled
                      ? healthy
                        ? 'default'
                        : 'destructive'
                      : 'secondary'
                }
                className="mt-1"
              >
                {unknown ? (
                  lastChecked ? (
                    'Status unavailable · stale'
                  ) : (
                    'Status unknown'
                  )
                ) : enabled ? (
                  healthy ? (
                    <>
                      <CheckCircle2 className="h-3 w-3 mr-1" />
                      Healthy
                    </>
                  ) : (
                    <>
                      <XCircle className="h-3 w-3 mr-1" />
                      Unhealthy
                    </>
                  )
                ) : (
                  <>
                    <XCircle className="h-3 w-3 mr-1" />
                    Disabled
                  </>
                )}
              </Badge>
            </div>
          </div>
          {enabled && (
            <Button
              variant="ghost"
              size="icon"
              onClick={onEdit}
              disabled={isPending}
              title="Edit configuration"
            >
              <Settings className="h-4 w-4" />
            </Button>
          )}
        </div>
        <CardDescription className="mt-3">{description}</CardDescription>
      </CardHeader>

      <CardContent className="flex-1 space-y-4">
        {error != null && (
          <ReadFailure
            embedded
            resource={`${name} status`}
            error={error}
            cached={lastChecked > 0}
            onRetry={onRetry}
            retrying={retrying}
          />
        )}
        <p className="text-xs text-muted-foreground">
          Last checked:{' '}
          {lastChecked
            ? new Date(lastChecked).toLocaleString()
            : 'Never successfully checked'}
        </p>
        {unknown && lastChecked > 0 && (
          <p className="text-sm text-muted-foreground">
            Last known state:{' '}
            {enabled ? (healthy ? 'Healthy' : 'Unhealthy') : 'Disabled'}
          </p>
        )}
        {!canManage && !unknown && (
          <p className="text-sm text-muted-foreground">
            Administrator permission is required to change platform services.
          </p>
        )}
        {enabled && (
          <div className="grid gap-2 sm:grid-cols-2">
            <div className="p-3 rounded-lg border bg-muted/30">
              <p className="text-xs text-muted-foreground">Version</p>
              <p className="font-medium text-sm mt-0.5">
                {version || 'Unknown'}
              </p>
            </div>
            <div className="p-3 rounded-lg border bg-muted/30">
              <p className="text-xs text-muted-foreground">Docker Image</p>
              <p className="font-medium text-sm mt-0.5 font-mono truncate">
                {dockerImage || 'Unknown'}
              </p>
            </div>
          </div>
        )}

        <div>
          <h4 className="text-sm font-medium mb-2">Features</h4>
          <ul className="text-sm text-muted-foreground space-y-1">
            {features.map((feature) => (
              <li key={feature} className="flex items-center gap-2">
                <span className="h-1.5 w-1.5 rounded-full bg-primary flex-shrink-0" />
                {feature}
              </li>
            ))}
          </ul>
        </div>
      </CardContent>

      <div className="p-6 pt-0">
        {enabled === undefined ? null : enabled ? (
          <Button
            variant="destructive"
            className="w-full gap-2"
            onClick={onDisable}
            disabled={isPending}
          >
            {isDisabling ? (
              <>
                <Loader2 className="h-4 w-4 animate-spin" />
                Disabling...
              </>
            ) : (
              <>
                <PowerOff className="h-4 w-4" />
                Disable {name}
              </>
            )}
          </Button>
        ) : (
          <Button
            className="w-full gap-2"
            onClick={onEnable}
            disabled={isPending}
          >
            {isEnabling ? (
              <>
                <Loader2 className="h-4 w-4 animate-spin" />
                Enabling...
              </>
            ) : (
              <>
                <Power className="h-4 w-4" />
                Enable {name}
              </>
            )}
          </Button>
        )}
      </div>
    </Card>
  )
}

function ServiceCardSkeleton() {
  return (
    <Card>
      <CardHeader>
        <div className="flex items-center gap-3">
          <Skeleton className="h-10 w-10 rounded-lg" />
          <div className="space-y-2">
            <Skeleton className="h-5 w-24" />
            <Skeleton className="h-5 w-16" />
          </div>
        </div>
        <Skeleton className="h-4 w-full mt-3" />
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="grid gap-2 sm:grid-cols-2">
          <Skeleton className="h-16 w-full" />
          <Skeleton className="h-16 w-full" />
        </div>
        <div className="space-y-2">
          <Skeleton className="h-4 w-20" />
          <Skeleton className="h-3 w-full" />
          <Skeleton className="h-3 w-full" />
          <Skeleton className="h-3 w-3/4" />
        </div>
      </CardContent>
      <div className="p-6 pt-0">
        <Skeleton className="h-10 w-full" />
      </div>
    </Card>
  )
}
