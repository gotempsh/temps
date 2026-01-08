import { useState, useEffect } from 'react'
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

type ServiceType = 'kv' | 'blob'

interface EditDockerImageDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  serviceName: string
  currentImage: string
  onSave: (newImage: string) => void
  isPending: boolean
}

function EditDockerImageDialog({
  open,
  onOpenChange,
  serviceName,
  currentImage,
  onSave,
  isPending,
}: EditDockerImageDialogProps) {
  const [dockerImage, setDockerImage] = useState(currentImage)

  // Sync state when dialog opens with new currentImage
  useEffect(() => {
    if (open) {
      setDockerImage(currentImage)
    }
  }, [open, currentImage])

  const handleSave = () => {
    if (dockerImage.trim()) {
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
              onChange={(e) => setDockerImage(e.target.value)}
              placeholder={serviceName === 'KV Store' ? 'redis:8-alpine' : 'minio/minio:latest'}
            />
            <p className="text-xs text-muted-foreground">
              {serviceName === 'KV Store'
                ? 'Examples: redis:8-alpine, redis:8-alpine, valkey/valkey:8-alpine'
                : 'Examples: minio/minio:latest, minio/minio:RELEASE.2025-09-07T16-13-09Z'}
            </p>
          </div>
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)} disabled={isPending}>
            Cancel
          </Button>
          <Button onClick={handleSave} disabled={isPending || !dockerImage.trim()}>
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
  const [editDialogOpen, setEditDialogOpen] = useState(false)
  const [editingService, setEditingService] = useState<ServiceType | null>(null)

  // Fetch KV status
  const { data: kvStatus, isLoading: kvLoading } = useQuery({
    ...kvStatusOptions(),
    refetchInterval: 10000,
  })

  // Fetch Blob status
  const { data: blobStatus, isLoading: blobLoading } = useQuery({
    ...blobStatusOptions(),
    refetchInterval: 10000,
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

  const isLoading = kvLoading || blobLoading

  const handleEditKv = () => {
    setEditingService('kv')
    setEditDialogOpen(true)
  }

  const handleEditBlob = () => {
    setEditingService('blob')
    setEditDialogOpen(true)
  }

  const handleSaveDockerImage = (newImage: string) => {
    if (editingService === 'kv') {
      kvUpdateMut.mutate({ body: { docker_image: newImage } })
    } else if (editingService === 'blob') {
      blobUpdateMut.mutate({ body: { docker_image: newImage } })
    }
  }

  if (isLoading) {
    return (
      <div className="space-y-6">
        <div className="grid gap-6 md:grid-cols-2">
          <ServiceCardSkeleton />
          <ServiceCardSkeleton />
        </div>
      </div>
    )
  }

  const currentServiceName = editingService === 'kv' ? 'KV Store' : 'Blob Storage'
  const currentImage =
    editingService === 'kv'
      ? kvStatus?.docker_image || 'redis:8-alpine'
      : blobStatus?.docker_image || 'minio/minio:latest'

  return (
    <div className="space-y-6">
      <Alert>
        <Info className="h-4 w-4" />
        <AlertTitle>Platform Services</AlertTitle>
        <AlertDescription>
          These services are shared across all projects. Each project's data is
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
          enabled={kvStatus?.enabled ?? false}
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
          enabled={blobStatus?.enabled ?? false}
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
        currentImage={currentImage}
        onSave={handleSaveDockerImage}
        isPending={kvUpdateMut.isPending || blobUpdateMut.isPending}
      />
    </div>
  )
}

interface ServiceCardProps {
  name: string
  description: string
  icon: React.ComponentType<{ className?: string }>
  enabled: boolean
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
  const isPending = isEnabling || isDisabling

  return (
    <Card className="flex flex-col">
      <CardHeader>
        <div className="flex items-start justify-between">
          <div className="flex items-center gap-3">
            <div className="p-2 rounded-lg bg-primary/10">
              <Icon className="h-6 w-6 text-primary" />
            </div>
            <div>
              <CardTitle className="text-lg">{name}</CardTitle>
              <Badge
                variant={enabled ? (healthy ? 'default' : 'destructive') : 'secondary'}
                className="mt-1"
              >
                {enabled ? (
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
              title="Edit configuration"
            >
              <Settings className="h-4 w-4" />
            </Button>
          )}
        </div>
        <CardDescription className="mt-3">{description}</CardDescription>
      </CardHeader>

      <CardContent className="flex-1 space-y-4">
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
        {enabled ? (
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
