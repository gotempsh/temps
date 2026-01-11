import { ProjectResponse } from '@/api/client'
import {
  kvStatusOptions,
  blobStatusOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { useEffect } from 'react'
import { Link } from 'react-router-dom'
import { useQuery } from '@tanstack/react-query'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { Database, HardDrive, ArrowRight, CheckCircle2, XCircle } from 'lucide-react'

interface ServicesOverviewProps {
  project: ProjectResponse
}

export function ServicesOverview({ project: _project }: ServicesOverviewProps) {
  const { setBreadcrumbs } = useBreadcrumbs()

  // Fetch service statuses
  const { data: kvStatus, isLoading: kvLoading } = useQuery({
    ...kvStatusOptions(),
  })

  const { data: blobStatus, isLoading: blobLoading } = useQuery({
    ...blobStatusOptions(),
  })

  useEffect(() => {
    setBreadcrumbs([{ label: 'Services' }])
  }, [setBreadcrumbs])

  const isLoading = kvLoading || blobLoading

  const services = [
    {
      id: 'kv',
      name: 'KV Store',
      description: 'Redis-backed key-value storage for caching, sessions, and real-time data',
      icon: Database,
      features: [
        'Fast in-memory storage',
        'TTL support for automatic expiration',
        'Atomic operations (INCR, DECR)',
        'Pattern-based key matching',
      ],
      useCases: ['Session storage', 'Caching', 'Rate limiting', 'Real-time counters'],
      enabled: kvStatus?.enabled ?? false,
      healthy: kvStatus?.healthy ?? false,
    },
    {
      id: 'blob',
      name: 'Blob Storage',
      description: 'S3-compatible object storage for files, images, and large data',
      icon: HardDrive,
      features: [
        'S3-compatible API',
        'Automatic content type detection',
        'Streaming uploads/downloads',
        'Prefix-based listing',
      ],
      useCases: ['File uploads', 'Image storage', 'Asset hosting', 'Backup storage'],
      enabled: blobStatus?.enabled ?? false,
      healthy: blobStatus?.healthy ?? false,
    },
  ]

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-xl font-semibold sm:text-2xl">Services</h1>
        <p className="text-muted-foreground mt-1">
          Platform services available for your project
        </p>
      </div>

      <div className="grid gap-6 md:grid-cols-2">
        {services.map((service) => {
          const Icon = service.icon
          return (
            <Card key={service.id} className="flex flex-col">
              <CardHeader>
                <div className="flex items-start justify-between">
                  <div className="flex items-center gap-3">
                    <div className="p-2 rounded-lg bg-primary/10">
                      <Icon className="h-6 w-6 text-primary" />
                    </div>
                    <div>
                      <CardTitle className="text-lg">{service.name}</CardTitle>
                      {isLoading ? (
                        <Skeleton className="h-5 w-20 mt-1" />
                      ) : (
                        <Badge
                          variant={service.enabled ? (service.healthy ? 'default' : 'destructive') : 'secondary'}
                          className="mt-1"
                        >
                          {service.enabled ? (
                            service.healthy ? (
                              <>
                                <CheckCircle2 className="h-3 w-3 mr-1" />
                                Enabled
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
                      )}
                    </div>
                  </div>
                </div>
                <CardDescription className="mt-3">
                  {service.description}
                </CardDescription>
              </CardHeader>
              <CardContent className="flex-1 space-y-4">
                <div>
                  <h4 className="text-sm font-medium mb-2">Features</h4>
                  <ul className="text-sm text-muted-foreground space-y-1">
                    {service.features.map((feature) => (
                      <li key={feature} className="flex items-center gap-2">
                        <span className="h-1.5 w-1.5 rounded-full bg-primary" />
                        {feature}
                      </li>
                    ))}
                  </ul>
                </div>
                <div>
                  <h4 className="text-sm font-medium mb-2">Use Cases</h4>
                  <div className="flex flex-wrap gap-1.5">
                    {service.useCases.map((useCase) => (
                      <Badge key={useCase} variant="outline" className="text-xs">
                        {useCase}
                      </Badge>
                    ))}
                  </div>
                </div>
              </CardContent>
              <div className="p-6 pt-0">
                <Link to={service.id}>
                  <Button className="w-full gap-2">
                    Configure {service.name}
                    <ArrowRight className="h-4 w-4" />
                  </Button>
                </Link>
              </div>
            </Card>
          )
        })}
      </div>
    </div>
  )
}
