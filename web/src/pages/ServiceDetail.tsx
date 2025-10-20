import {
  deleteServiceMutation,
  getServiceOptions,
  getServicePreviewEnvironmentVariablesMaskedOptions,
  startServiceMutation,
  stopServiceMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { CopyButton } from '@/components/ui/copy-button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { ServiceLogo } from '@/components/ui/service-logo'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { cn } from '@/lib/utils'
import { useMutation, useQuery } from '@tanstack/react-query'
import { format } from 'date-fns'
import {
  AlertCircle,
  ArrowLeft,
  Eye,
  EyeOff,
  Loader2,
  RefreshCcw,
  Trash2,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router-dom'

export function ServiceDetail() {
  const { id } = useParams<{ id: string }>()
  const { setBreadcrumbs } = useBreadcrumbs()
  const navigate = useNavigate()
  const [visibleValues, setVisibleValues] = useState<Record<string, boolean>>(
    {}
  )
  const [isDeleteDialogOpen, setIsDeleteDialogOpen] = useState(false)
  const [error, setError] = useState<string | null>(null)

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

  const toggleValueVisibility = (key: string) => {
    setVisibleValues((prev) => ({
      ...prev,
      [key]: !prev[key],
    }))
  }

  const maskValue = (value: string) => {
    return '*'.repeat(value.length)
  }

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
      refetch()
      setError(null)
    },
  })

  const deleteService = useMutation({
    ...deleteServiceMutation(),
    meta: {
      errorTitle: 'Failed to delete service',
    },
    onSuccess: () => {
      navigate('/storage')
    },
    onError: () => {
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
        <div className="flex items-center gap-4">
          <Link to="/storage">
            <Button variant="ghost" size="icon">
              <ArrowLeft className="h-4 w-4" />
            </Button>
          </Link>
          <h1 className="text-xl font-semibold sm:text-2xl">Service Details</h1>
        </div>

        {error && (
          <Alert variant="destructive">
            <AlertCircle className="h-4 w-4" />
            <AlertDescription>{error}</AlertDescription>
          </Alert>
        )}

        <div className="grid gap-6">
          <Card>
            <CardHeader>
              <div className="flex items-center justify-between">
                <div className="space-y-1">
                  <CardTitle className="flex items-center gap-2">
                    <ServiceLogo service={service.service.service_type} />
                    {service.service.name}
                  </CardTitle>
                  <CardDescription>
                    {service.service.service_type} • Created{' '}
                    {format(
                      new Date(service.service.created_at),
                      'MMM d, yyyy'
                    )}
                  </CardDescription>
                </div>
                <div className="flex items-center gap-2">
                  <Button
                    variant={
                      service.service.status === 'running'
                        ? 'destructive'
                        : 'default'
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
                    variant="ghost"
                    size="sm"
                    onClick={() => setIsDeleteDialogOpen(true)}
                    className="text-destructive hover:text-destructive hover:bg-destructive/10"
                  >
                    <Trash2 className="h-4 w-4" />
                  </Button>
                </div>
              </div>
            </CardHeader>
            <CardContent className="space-y-6">
              <div className="grid gap-4">
                <div>
                  <h3 className="text-sm font-medium mb-2">Status</h3>
                  <p className="text-sm text-muted-foreground capitalize">
                    {service.service.status}
                  </p>
                </div>
                <div>
                  <h3 className="text-sm font-medium mb-2">Service Type</h3>
                  <p className="text-sm text-muted-foreground">
                    {service.service.service_type}
                  </p>
                </div>
                <div>
                  <h3 className="text-sm font-medium mb-2">Created At</h3>
                  <p className="text-sm text-muted-foreground">
                    <TimeAgo date={service.service.created_at} />
                  </p>
                </div>
              </div>
            </CardContent>
          </Card>

          {service.current_parameters && (
            <Card>
              <CardHeader>
                <CardTitle>Current Parameters</CardTitle>
                <CardDescription>
                  Service configuration and connection details
                </CardDescription>
              </CardHeader>
              <CardContent>
                <div className="grid gap-4">
                  {Object.entries(service.current_parameters || {}).map(
                    ([key, value]) => (
                      <div key={key}>
                        <div className="flex items-center justify-between mb-2">
                          <h3 className="text-sm font-medium">{key}</h3>
                          <Button
                            variant="ghost"
                            size="sm"
                            onClick={() => toggleValueVisibility(key)}
                            className="h-8 w-8 p-0"
                          >
                            {visibleValues[key] ? (
                              <EyeOff className="h-4 w-4" />
                            ) : (
                              <Eye className="h-4 w-4" />
                            )}
                          </Button>
                        </div>
                        <p className="text-sm text-muted-foreground font-mono bg-muted p-2 rounded-md">
                          {visibleValues[key]
                            ? (value as string)
                            : maskValue(value as string)}
                        </p>
                      </div>
                    )
                  )}
                </div>
              </CardContent>
            </Card>
          )}

          {/* Environment Variables Section */}
          <Card>
            <CardHeader>
              <div className="flex items-center justify-between">
                <div>
                  <CardTitle>Environment Variables</CardTitle>
                  <CardDescription>
                    Masked preview of environment variables available to
                    projects using this service
                  </CardDescription>
                </div>
                {envVars && Object.keys(envVars).length > 0 && (
                  <CopyButton
                    value={Object.entries(envVars)
                      .map(([key, value]) => `${key}=${value}`)
                      .join('\n')}
                  >
                    Copy All
                  </CopyButton>
                )}
              </div>
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
              ) : envVars && Object.keys(envVars).length > 0 ? (
                <div className="space-y-3">
                  <div className="flex items-center justify-between">
                    <p className="text-xs text-muted-foreground">
                      {Object.keys(envVars).length} environment variable
                      {Object.keys(envVars).length !== 1 ? 's' : ''} available
                    </p>
                  </div>

                  {/* Code block style display */}
                  <div className="relative">
                    <pre
                      className={cn(
                        'bg-muted/30 border rounded-md p-3 text-sm font-mono',
                        'max-h-60 overflow-y-auto overflow-x-auto',
                        'whitespace-pre-wrap break-all'
                      )}
                    >
                      {Object.entries(envVars).map(([key, value], index) => (
                        <span key={key}>
                          <span className="text-primary font-medium">
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

                  <p className="text-xs text-muted-foreground text-center">
                    These variables are automatically available to projects that
                    use this service
                  </p>
                </div>
              ) : (
                <div className="text-center py-8">
                  <p className="text-sm text-muted-foreground">
                    No environment variables available
                  </p>
                </div>
              )}
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
    </div>
  )
}
