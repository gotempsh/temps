import { listAvailableContainersOptions } from '@/api/client/@tanstack/react-query.gen'
import { AvailableContainerInfo } from '@/api/client/types.gen'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import { ServiceLogo } from '@/components/ui/service-logo'
import { getServiceTypeWithFallback } from '@/lib/service-type-detector'
import { useQuery } from '@tanstack/react-query'
import { AlertCircle, CheckCircle, Loader2 } from 'lucide-react'
import { useMemo } from 'react'

interface ContainerSelectorProps {
  onContainerSelected: (container: AvailableContainerInfo) => void
}

export function ContainerSelector({
  onContainerSelected,
}: ContainerSelectorProps) {
  const {
    data: containers,
    isLoading,
    error,
  } = useQuery({
    ...listAvailableContainersOptions(),
  })

  // Memoize service type extraction for each container
  const containerServiceTypes = useMemo(() => {
    if (!containers) return {}
    return containers.reduce(
      (acc, container) => {
        acc[container.container_id] = getServiceTypeWithFallback(
          container.service_type,
          container.image
        )
        return acc
      },
      {} as Record<string, string | null>
    )
  }, [containers])

  if (isLoading) {
    return (
      <div className="flex items-center justify-center py-8">
        <Loader2 className="h-6 w-6 animate-spin text-muted-foreground" />
      </div>
    )
  }

  if (error) {
    return (
      <Alert variant="destructive">
        <AlertCircle className="h-4 w-4" />
        <AlertDescription>
          Failed to load available containers. Please try again.
        </AlertDescription>
      </Alert>
    )
  }

  if (!containers || containers.length === 0) {
    return (
      <Alert>
        <AlertCircle className="h-4 w-4" />
        <AlertDescription>
          No containers available to import. Make sure you have running
          containers in your environment.
        </AlertDescription>
      </Alert>
    )
  }

  return (
    <div className="space-y-3">
      <p className="text-sm text-muted-foreground">
        Select a running container to import as a service:
      </p>
      <div className="space-y-2 max-h-[400px] overflow-y-auto">
        {containers.map((container) => (
          <Card
            key={container.container_id}
            className="p-4 cursor-pointer hover:bg-muted/50 transition-colors"
            onClick={() => onContainerSelected(container)}
          >
            <div className="flex items-center justify-between gap-3">
              <div className="flex items-center gap-3 flex-1 min-w-0">
                {containerServiceTypes[container.container_id] && (
                  <ServiceLogo
                    service={
                      containerServiceTypes[container.container_id] as any
                    }
                    className="h-10 w-10 shrink-0"
                  />
                )}
                <div className="flex-1 min-w-0">
                  <h3 className="font-medium text-sm">
                    {container.container_name}
                  </h3>
                  <p className="text-xs text-muted-foreground font-mono truncate">
                    {container.image}
                  </p>
                  <p className="text-xs text-muted-foreground">
                    {container.container_id.substring(0, 12)}
                  </p>
                </div>
              </div>
              <div className="flex items-center gap-2 shrink-0">
                {container.is_running && (
                  <CheckCircle className="h-4 w-4 text-green-500" />
                )}
                <Button
                  variant="outline"
                  size="sm"
                  onClick={(e) => {
                    e.stopPropagation()
                    onContainerSelected(container)
                  }}
                >
                  Select
                </Button>
              </div>
            </div>
          </Card>
        ))}
      </div>
    </div>
  )
}
