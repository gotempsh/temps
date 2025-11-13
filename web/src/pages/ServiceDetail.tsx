import {
  deleteServiceMutation,
  getServiceOptions,
  getServicePreviewEnvironmentVariablesMaskedOptions,
  listServiceProjectsOptions,
  startServiceMutation,
  stopServiceMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { EditServiceDialog } from '@/components/storage/EditServiceDialog'
import { UpgradeServiceDialog } from '@/components/storage/UpgradeServiceDialog'
import { Alert, AlertDescription } from '@/components/ui/alert'
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
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { EnvVariablesDisplay } from '@/components/ui/env-variables-display'
import { ServiceLogo } from '@/components/ui/service-logo'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { maskValue, shouldMaskValue } from '@/lib/masking'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  AlertCircle,
  ArrowLeft,
  ArrowUpCircle,
  Eye,
  EyeOff,
  Loader2,
  Pencil,
  RefreshCcw,
  Trash2,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import { toast } from 'sonner'

export function ServiceDetail() {
  const { id } = useParams<{ id: string }>()
  const { setBreadcrumbs } = useBreadcrumbs()
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const [isDeleteDialogOpen, setIsDeleteDialogOpen] = useState(false)
  const [isUpgradeDialogOpen, setIsUpgradeDialogOpen] = useState(false)
  const [isEditDialogOpen, setIsEditDialogOpen] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [visibleParameters, setVisibleParameters] = useState<Set<string>>(
    new Set()
  )

  const {
    data: service,
    isLoading,
    error: queryError,
    refetch,
  } = useQuery({
    ...getServiceOptions({
      path: { id: parseInt(id!) },
    }),
    enabled: !!id,
  })

  // Query for environment variables
  const {
    data: envVars,
    isLoading: envVarsLoading,
    error: envVarsError,
  } = useQuery({
    ...getServicePreviewEnvironmentVariablesMaskedOptions({
      path: { id: parseInt(id!) },
    }),
    enabled: !!id,
    staleTime: 5 * 60 * 1000, // Cache for 5 minutes
  })

  // Query for linked projects
  const { data: linkedProjectsResponse, isLoading: linkedProjectsLoading } =
    useQuery({
      ...listServiceProjectsOptions({
        path: { id: parseInt(id!) },
      }),
      enabled: !!id,
    })

  useEffect(() => {
    if (service) {
      setBreadcrumbs([
        { label: 'Storage', href: '/storage' },
        {
          label: service.service.name || 'Service Details',
          href: `/storage/${id}`,
        },
      ])
    } else {
      setBreadcrumbs([
        { label: 'Storage', href: '/storage' },
        { label: 'Service Details', href: `/storage/${id}` },
      ])
    }
  }, [setBreadcrumbs, id, service])

  usePageTitle(service?.service?.name || 'Service Details')

  const startService = useMutation({
    ...startServiceMutation(),
    meta: {
      errorTitle: 'Failed to start service',
    },
    onSuccess: () => {
      refetch()
      setError(null)
    },
  })

  const stopService = useMutation({
    ...stopServiceMutation(),
    meta: {
      errorTitle: 'Failed to stop service',
    },
    onSuccess: () => {
      toast.success('Service stopped successfully')
      refetch()
    },
  })

  const deleteService = useMutation({
    ...deleteServiceMutation(),
    meta: {
      errorTitle: 'Failed to delete service',
    },
    onSuccess: () => {
      toast.success('Service deleted successfully')
      navigate('/storage')
    },
    onError: (error: any) => {
      toast.error('Failed to delete service', {
        description:
          error.detail || error.message || 'An unexpected error occurred',
      })
      setIsDeleteDialogOpen(false)
    },
  })

  const handleServiceAction = async () => {
    if (!service) return

    if (service.service.status === 'running') {
      stopService.mutate({ path: { id: parseInt(id!) } })
    } else if (service.service.status === 'stopped') {
      startService.mutate({ path: { id: parseInt(id!) } })
    }
  }

  const handleDelete = async () => {
    deleteService.mutate({ path: { id: parseInt(id!) } })
  }

  if (isLoading) {
    return (
      <div className="flex-1 overflow-auto">
        <div className="sm:p-4 space-y-6 md:p-6">
          <div className="h-8 w-32 bg-muted rounded animate-pulse" />
          <Card>
            <CardHeader>
              <div className="space-y-2">
                <div className="h-5 w-40 bg-muted rounded animate-pulse" />
                <div className="h-4 w-24 bg-muted rounded animate-pulse" />
              </div>
            </CardHeader>
            <CardContent>
              <div className="space-y-4">
                <div className="h-4 w-full bg-muted rounded animate-pulse" />
                <div className="h-4 w-3/4 bg-muted rounded animate-pulse" />
              </div>
            </CardContent>
          </Card>
        </div>
      </div>
    )
  }

  if (queryError || !service) {
    return (
      <div className="flex-1 overflow-auto">
        <div className="sm:p-4 space-y-6 md:p-6">
          <div className="flex flex-col items-center justify-center py-12 text-center">
            <p className="text-sm text-muted-foreground mb-4">
              Failed to load service details
            </p>
            <Button
              variant="outline"
              onClick={() => refetch()}
              className="gap-2"
            >
              <RefreshCcw className="h-4 w-4" />
              Try again
            </Button>
          </div>
        </div>
      </div>
    )
  }

  return (
    <div className="flex-1 overflow-auto">
      <div className="sm:p-4 space-y-6 md:p-6">
        <div className="flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between">
          <div className="flex items-center gap-3">
            <Link to="/storage">
              <Button variant="ghost" size="icon">
                <ArrowLeft className="h-4 w-4" />
              </Button>
            </Link>
            <ServiceLogo
              service={service.service.service_type}
              className="h-8 w-8"
            />
            <div className="flex flex-col gap-2">
              <div className="flex items-center gap-2 flex-wrap">
                <h1 className="text-xl font-semibold sm:text-2xl">
                  {service.service.name}
                </h1>
                <Badge
                  variant={
                    service.service.status === 'running'
                      ? 'default'
                      : service.service.status === 'stopped'
                        ? 'secondary'
                        : 'outline'
                  }
                  className="capitalize"
                >
                  {service.service.status}
                </Badge>
                <Badge variant="outline" className="gap-1.5">
                  <ServiceLogo
                    service={service.service.service_type}
                    className="h-3 w-3"
                  />
                  {service.service.service_type}
                </Badge>
              </div>
              <p className="text-sm text-muted-foreground">
                Created <TimeAgo date={service.service.created_at} />
              </p>
            </div>
          </div>

          <div className="flex items-center gap-2 self-start sm:self-auto">
            <Button
              variant={
                service.service.status === 'running' ? 'destructive' : 'default'
              }
              size="sm"
              disabled={
                service.service.status === 'creating' ||
                startService.isPending ||
                stopService.isPending
              }
              onClick={handleServiceAction}
            >
              {(startService.isPending || stopService.isPending) && (
                <Loader2 className="h-4 w-4 mr-2 animate-spin" />
              )}
              {service.service.status === 'running'
                ? 'Stop'
                : service.service.status === 'creating'
                  ? 'Creating...'
                  : 'Start'}
            </Button>
            <Button
              variant="outline"
              size="sm"
              onClick={() => setIsEditDialogOpen(true)}
              className="gap-2"
            >
              <Pencil className="h-4 w-4" />
              Edit
            </Button>
            <Button
              variant="outline"
              size="sm"
              onClick={() => setIsUpgradeDialogOpen(true)}
              className="gap-2"
            >
              <ArrowUpCircle className="h-4 w-4" />
              Upgrade
            </Button>
            <Button
              variant="ghost"
              size="sm"
              onClick={() => setIsDeleteDialogOpen(true)}
              className="text-destructive hover:text-destructive hover:bg-destructive/10"
            >
              <Trash2 className="h-4 w-4" />
            </Button>
          </div>
        </div>

        {error && (
          <Alert variant="destructive">
            <AlertCircle className="h-4 w-4" />
            <AlertDescription>{error}</AlertDescription>
          </Alert>
        )}

        <div className="grid gap-6">
          {/* Linked Projects Section */}
          <Card>
            <CardHeader>
              <CardTitle className="flex items-center gap-2">
                <span>Linked Projects</span>
                <Badge variant="outline">
                  {linkedProjectsLoading ? (
                    <Loader2 className="h-3 w-3 animate-spin" />
                  ) : (
                    linkedProjectsResponse?.length || 0
                  )}
                </Badge>
              </CardTitle>
              <CardDescription>
                Projects that are using this service
              </CardDescription>
            </CardHeader>
            <CardContent>
              {linkedProjectsLoading ? (
                <div className="flex items-center justify-center py-8">
                  <Loader2 className="h-4 w-4 animate-spin mr-2" />
                  <span className="text-sm text-muted-foreground">
                    Loading projects...
                  </span>
                </div>
              ) : linkedProjectsResponse &&
                linkedProjectsResponse.length > 0 ? (
                <div className="space-y-2">
                  {linkedProjectsResponse.map((link) => (
                    <div
                      key={link.id}
                      className="flex items-center justify-between p-3 rounded-md border border-border hover:bg-muted/50 transition-colors"
                    >
                      <div className="flex flex-col">
                        <p className="font-medium text-sm">
                          {link.project.slug}
                        </p>
                        <p className="text-xs text-muted-foreground">
                          Linked <TimeAgo date={link.service.created_at} />
                        </p>
                      </div>
                      <Link to={`/projects/${link.project.slug}`}>
                        <Button variant="ghost" size="sm" className="gap-2">
                          <ArrowLeft className="h-4 w-4 rotate-180" />
                          View Project
                        </Button>
                      </Link>
                    </div>
                  ))}
                </div>
              ) : (
                <div className="text-sm text-muted-foreground text-center py-8">
                  No projects are currently using this service
                </div>
              )}
            </CardContent>
          </Card>

          {/* Service Configuration Section */}
          <Card>
            <CardHeader>
              <CardTitle>Configuration</CardTitle>
              <CardDescription>Current service parameters</CardDescription>
            </CardHeader>
            <CardContent>
              {service.current_parameters &&
              Object.keys(service.current_parameters).length > 0 ? (
                <div className="space-y-4">
                  {Object.entries(service.current_parameters).map(
                    ([key, value]) => {
                      const isSensitive = shouldMaskValue(key)
                      const isVisible = visibleParameters.has(key)
                      const displayValue =
                        isSensitive && !isVisible ? maskValue(value) : value

                      return (
                        <div key={key} className="space-y-1.5">
                          <div className="text-sm font-medium capitalize">
                            {key
                              .replace(/_/g, ' ')
                              .replace(/\b\w/g, (char) => char.toUpperCase())}
                          </div>
                          <div className="flex items-center gap-2 rounded-md border border-border bg-muted/50 p-3">
                            <span className="flex-1 break-all text-foreground font-mono text-sm">
                              {displayValue || (
                                <span className="text-muted-foreground">-</span>
                              )}
                            </span>
                            {isSensitive && (
                              <Button
                                variant="ghost"
                                size="sm"
                                onClick={() => {
                                  setVisibleParameters((prev) => {
                                    const next = new Set(prev)
                                    if (next.has(key)) {
                                      next.delete(key)
                                    } else {
                                      next.add(key)
                                    }
                                    return next
                                  })
                                }}
                                className="flex-shrink-0"
                                title={isVisible ? 'Hide value' : 'Show value'}
                              >
                                {isVisible ? (
                                  <EyeOff className="h-4 w-4" />
                                ) : (
                                  <Eye className="h-4 w-4" />
                                )}
                              </Button>
                            )}
                          </div>
                        </div>
                      )
                    }
                  )}
                </div>
              ) : (
                <div className="text-sm text-muted-foreground">
                  No parameters configured
                </div>
              )}
            </CardContent>
          </Card>

          {/* Environment Variables Section */}
          <Card>
            <CardHeader>
              <CardTitle>Environment Variables</CardTitle>
              <CardDescription>
                Preview of environment variables available to projects using
                this service
              </CardDescription>
            </CardHeader>
            <CardContent>
              {envVarsLoading ? (
                <div className="flex items-center justify-center py-8">
                  <Loader2 className="h-4 w-4 animate-spin mr-2" />
                  <span className="text-sm text-muted-foreground">
                    Loading environment variables...
                  </span>
                </div>
              ) : envVarsError ? (
                <div className="text-center py-8">
                  <AlertCircle className="h-8 w-8 mx-auto text-muted-foreground mb-2" />
                  <p className="text-sm text-muted-foreground">
                    Failed to load environment variables
                  </p>
                </div>
              ) : envVars ? (
                <>
                  <EnvVariablesDisplay
                    variables={envVars}
                    showCopy={true}
                    showMaskToggle={true}
                    defaultMasked={true}
                    maxHeight="20rem"
                  />
                  <p className="text-xs text-muted-foreground text-center mt-3">
                    These variables are automatically available to projects that
                    use this service
                  </p>
                </>
              ) : null}
            </CardContent>
          </Card>
        </div>
      </div>

      <Dialog open={isDeleteDialogOpen} onOpenChange={setIsDeleteDialogOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Delete Service</DialogTitle>
            <DialogDescription>
              Are you sure you want to delete this service? This action cannot
              be undone.
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => setIsDeleteDialogOpen(false)}
            >
              Cancel
            </Button>
            <Button
              variant="destructive"
              onClick={handleDelete}
              disabled={deleteService.isPending}
            >
              {deleteService.isPending && (
                <Loader2 className="h-4 w-4 mr-2 animate-spin" />
              )}
              Delete
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <UpgradeServiceDialog
        open={isUpgradeDialogOpen}
        onOpenChange={setIsUpgradeDialogOpen}
        serviceId={parseInt(id!)}
        serviceName={service.service.name}
        currentImage={service.current_parameters?.docker_image || undefined}
        serviceType={service.service.service_type}
      />

      <EditServiceDialog
        open={isEditDialogOpen}
        onOpenChange={setIsEditDialogOpen}
        service={service.service}
        currentParameters={service.current_parameters}
        onSuccess={() => {
          refetch()
          queryClient.invalidateQueries({
            queryKey: getServiceOptions({
              path: { id: parseInt(id!) },
            }).queryKey,
          })
        }}
      />
    </div>
  )
}
