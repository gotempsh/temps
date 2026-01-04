import {
  kvStatusOptions,
  kvEnableMutation,
  kvDisableMutation,
  blobStatusOptions,
  blobEnableMutation,
  blobDisableMutation,
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
  Database,
  HardDrive,
  CheckCircle2,
  XCircle,
  Loader2,
  Info,
  Power,
  PowerOff,
} from 'lucide-react'
import { Skeleton } from '@/components/ui/skeleton'
import { toast } from 'sonner'

export function PlatformServices() {
  const queryClient = useQueryClient()

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

  const isLoading = kvLoading || blobLoading

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
          isEnabling={blobEnableMut.isPending}
          isDisabling={blobDisableMut.isPending}
        />
      </div>
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
