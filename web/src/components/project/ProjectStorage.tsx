import { ExternalServiceInfo, ProjectResponse } from '@/api/client'
import {
  getServicePreviewEnvironmentVariablesMaskedOptions,
  linkServiceToProjectMutation,
  listProjectServicesOptions,
  listServicesOptions,
  unlinkServiceFromProjectMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { CreateServiceButton } from '@/components/storage/CreateServiceButton'
import EmptyStateStorage from '@/components/storage/EmptyStateStorage'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from '@/components/ui/collapsible'
import { ServiceLogo } from '@/components/ui/service-logo'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { cn } from '@/lib/utils'
import { useMutation, useQuery } from '@tanstack/react-query'
import {
  ArrowRight,
  ChevronDown,
  ChevronRight,
  Eye,
  EyeOff,
  Loader2,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { Link } from 'react-router-dom'
import { toast } from 'sonner'
import { CopyButton } from '../ui/copy-button'
import { TimeAgo } from '@/components/utils/TimeAgo'

function ServiceCard({
  service,
  isLinked,
  onToggle,
}: {
  service: ExternalServiceInfo
  isLinked: boolean
  onToggle: () => Promise<void>
}) {
  const [isEnvPreviewOpen, setIsEnvPreviewOpen] = useState(false)
  const [showEnvPreview, setShowEnvPreview] = useState(false)

  const {
    data: envVars,
    isLoading: envVarsLoading,
    error: envVarsError,
  } = useQuery({
    ...getServicePreviewEnvironmentVariablesMaskedOptions({
      path: { id: service.id },
    }),
    enabled: isEnvPreviewOpen && isLinked, // Only load when expanded and service is linked
    staleTime: 5 * 60 * 1000, // Cache for 5 minutes
  })

  // Auto-show environment variables when section is expanded and data is loaded
  useEffect(() => {
    if (isEnvPreviewOpen && envVars) {
      setShowEnvPreview(true)
    }
  }, [isEnvPreviewOpen, envVars])

  const handleEnvPreviewToggle = () => {
    setShowEnvPreview(!showEnvPreview)
  }

  const envVarCount = envVars ? Object.keys(envVars).length : 0
  return (
    <Collapsible open={isEnvPreviewOpen} onOpenChange={setIsEnvPreviewOpen}>
      <Card>
        <CardHeader>
          <div className="flex flex-col sm:flex-row sm:items-center sm:justify-between gap-4">
            <div className="flex-1 space-y-1">
              <CardTitle className="flex items-center gap-2">
                <ServiceLogo service={service.service_type} />
                {service.name}
                {isLinked && (
                  <CollapsibleTrigger asChild>
                    <Button
                      type="button"
                      variant="ghost"
                      size="sm"
                      className="h-6 w-6 p-0 ml-2 text-muted-foreground hover:text-foreground"
                    >
                      {isEnvPreviewOpen ? (
                        <ChevronDown className="h-4 w-4" />
                      ) : (
                        <ChevronRight className="h-4 w-4" />
                      )}
                    </Button>
                  </CollapsibleTrigger>
                )}
              </CardTitle>
              <CardDescription className="flex items-center gap-2 flex-wrap">
                <span>{service.service_type}</span>
                {service.created_at && (
                  <>
                    <span>•</span>
                    <span className="text-xs">
                      Created <TimeAgo date={service.created_at} />
                    </span>
                  </>
                )}
                <span>•</span>
                <Badge variant={isLinked ? 'default' : 'secondary'}>
                  {isLinked ? 'Linked' : 'Available'}
                </Badge>
                {isLinked && envVars && (
                  <Badge variant="secondary" className="text-xs">
                    {envVarCount} env var{envVarCount !== 1 ? 's' : ''}
                  </Badge>
                )}
              </CardDescription>
            </div>
            <div className="flex flex-col sm:flex-row items-start sm:items-center gap-2">
              {isLinked ? (
                <>
                  {isEnvPreviewOpen && (
                    <Link to={`/storage/${service.id}`}>
                      <Button variant="outline" size="sm" className="gap-2">
                        View Details
                        <ArrowRight className="h-4 w-4" />
                      </Button>
                    </Link>
                  )}
                  <Button variant="destructive" size="sm" onClick={onToggle}>
                    Unlink
                  </Button>
                </>
              ) : (
                <Button size="sm" onClick={onToggle}>
                  Link
                </Button>
              )}
            </div>
          </div>
        </CardHeader>

        {/* Environment Variables Preview Section */}
        <CollapsibleContent>
          {isLinked && (
            <CardContent className="pt-0 border-t">
              <div className="space-y-4">
                <div className="flex items-center justify-between">
                  <h4 className="text-sm font-medium text-muted-foreground">
                    Environment Variables
                  </h4>
                  <div className="flex items-center gap-3">
                    {envVars && showEnvPreview && (
                      <CopyButton
                        value={Object.entries(envVars)
                          .map(([key, value]) => `${key}=${value}`)
                          .join('\n')}
                      />
                    )}
                    <Button
                      type="button"
                      variant="ghost"
                      size="sm"
                      onClick={handleEnvPreviewToggle}
                      className="h-8 text-xs text-muted-foreground hover:text-foreground gap-2 px-3"
                    >
                      {showEnvPreview ? (
                        <>
                          <EyeOff className="h-3.5 w-3.5" />
                          Hide
                        </>
                      ) : (
                        <>
                          <Eye className="h-3.5 w-3.5" />
                          Show
                        </>
                      )}
                    </Button>
                  </div>
                </div>

                {envVarsLoading && (
                  <div className="flex items-center justify-center py-8 text-muted-foreground">
                    <Loader2 className="h-4 w-4 animate-spin mr-2" />
                    <span className="text-sm">
                      Loading environment variables...
                    </span>
                  </div>
                )}

                {envVarsError && (
                  <div className="text-center py-8">
                    <p className="text-sm text-muted-foreground">
                      Unable to load environment variables
                    </p>
                  </div>
                )}

                {!showEnvPreview && !envVarsLoading && !envVarsError && (
                  <div className="text-center py-8">
                    <p className="text-sm text-muted-foreground">
                      Click &quot;Show&quot; to preview environment variables
                    </p>
                  </div>
                )}

                {envVars && showEnvPreview && (
                  <div className="space-y-3">
                    {/* Code block style display */}
                    <div className="relative">
                      <pre
                        className={cn(
                          'bg-muted/50 border rounded-lg p-4 text-sm font-mono',
                          'max-h-48 overflow-y-auto',
                          'whitespace-pre-wrap leading-tight'
                        )}
                      >
                        {Object.entries(envVars).map(([key, value], index) => (
                          <span key={key}>
                            <span className="text-blue-600 dark:text-blue-400 font-medium">
                              {key}
                            </span>
                            <span className="text-muted-foreground">=</span>
                            <span className="text-foreground">{value}</span>
                            {index < Object.entries(envVars).length - 1
                              ? '\n'
                              : ''}
                          </span>
                        ))}
                      </pre>
                    </div>

                    <div className="flex items-center justify-center">
                      <Badge
                        variant="outline"
                        className="text-xs text-muted-foreground"
                      >
                        Masked for security • Available in project runtime
                      </Badge>
                    </div>
                  </div>
                )}
              </div>
            </CardContent>
          )}
        </CollapsibleContent>
      </Card>
    </Collapsible>
  )
}

export function ProjectStorage({ project }: { project: ProjectResponse }) {
  const { setBreadcrumbs } = useBreadcrumbs()

  useEffect(() => {
    setBreadcrumbs([{ label: 'Storage', href: '/storage' }])
  }, [setBreadcrumbs])

  const {
    data: services,
    isLoading: isLoadingServices,
    refetch: refetchServices,
  } = useQuery({
    ...listServicesOptions(),
  })

  const { data: servicesLinked, refetch: refetchServicesLinked } = useQuery({
    ...listProjectServicesOptions({
      path: {
        project_id: project.id,
      },
    }),
  })

  const linkServiceMutation = useMutation({
    ...linkServiceToProjectMutation(),
    meta: {
      errorTitle: 'Failed to link service to project',
    },
    onSuccess: () => {
      refetchServicesLinked()
    },
  })

  const unlinkServiceMutation = useMutation({
    ...unlinkServiceFromProjectMutation(),
    meta: {
      errorTitle: 'Failed to unlink service from project',
    },
    onSuccess: () => {
      refetchServicesLinked()
    },
  })

  const handleServiceToggle = async (serviceId: number) => {
    const isLinked = servicesLinked?.some((s) => s.service.id === serviceId)

    if (isLinked) {
      const promise = unlinkServiceMutation.mutateAsync({
        path: {
          id: serviceId,
          project_id: project.id,
        },
      })
      await toast.promise(promise, {
        loading: 'Unlinking service...',
        success: 'Service unlinked successfully',
        error: 'Failed to unlink service',
      })
    } else {
      const promise = linkServiceMutation.mutateAsync({
        path: {
          id: serviceId,
        },
        body: {
          project_id: project.id,
        },
      })
      await toast.promise(promise, {
        loading: 'Linking service...',
        success: 'Service linked successfully',
        error: 'Failed to link service',
      })
    }

    await refetchServicesLinked()
  }

  if (isLoadingServices) {
    return (
      <div className="flex-1 p-6">
        <div className="animate-pulse space-y-4">
          <div className="h-8 w-1/4 bg-muted rounded" />
          <div className="space-y-4">
            {[...Array(3)].map((_, i) => (
              <Card key={i}>
                <CardHeader>
                  <div className="flex items-center justify-between">
                    <div className="space-y-2">
                      <div className="h-5 w-40 bg-muted rounded" />
                      <div className="h-4 w-24 bg-muted rounded" />
                    </div>
                    <div className="h-8 w-20 bg-muted rounded" />
                  </div>
                </CardHeader>
              </Card>
            ))}
          </div>
        </div>
      </div>
    )
  }

  if (!services?.length) {
    return (
      <div className="flex-1 overflow-auto">
        <div className="sm:p-4 space-y-6 md:p-6">
          <div className="flex items-center justify-between mb-6">
            <h1 className="text-xl font-semibold sm:text-2xl">Storage</h1>
            <CreateServiceButton
              onSuccess={() => {
                refetchServices()
                refetchServicesLinked()
              }}
            />
          </div>
          <EmptyStateStorage />
        </div>
      </div>
    )
  }

  return (
    <div className="flex-1 overflow-auto">
      <div className="sm:p-4 space-y-6 md:p-6">
        <div className="flex items-center justify-between">
          <h1 className="text-xl font-semibold sm:text-2xl">Storage</h1>
          <CreateServiceButton
            onSuccess={() => {
              refetchServices()
              refetchServicesLinked()
            }}
          />
        </div>

        <div className="grid gap-4">
          {services.map((service) => (
            <ServiceCard
              key={service.id}
              service={service}
              isLinked={
                servicesLinked?.some((s) => s.service.id === service.id) ??
                false
              }
              onToggle={() => handleServiceToggle(service.id)}
            />
          ))}
        </div>
      </div>
    </div>
  )
}
