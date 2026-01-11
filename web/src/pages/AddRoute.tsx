import {
  createRouteMutation,
  listDomainsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import {
  Form,
  FormControl,
  FormDescription,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { usePlatformCapabilities } from '@/hooks/usePlatformCapabilities'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery } from '@tanstack/react-query'
import {
  ArrowLeft,
  ArrowRight,
  Globe,
  Info,
  Layers,
  Lock,
  Router,
  Server,
} from 'lucide-react'
import { useEffect, useMemo } from 'react'
import { useForm, useWatch } from 'react-hook-form'
import { useNavigate } from 'react-router-dom'
import { toast } from 'sonner'
import { z } from 'zod'

const addRouteSchema = z
  .object({
    routeType: z.enum(['http', 'tls']),
    domainInputType: z.enum(['select', 'manual']),
    domain: z.string().min(1, 'Domain is required'),
    subdomain: z.string().optional(),
    host: z.string().min(1, 'Host is required'),
    port: z.number().min(1, 'Port must be at least 1').max(65535, 'Port must be at most 65535'),
  })
  .refine(
    (data) => {
      // Only require subdomain when selecting a wildcard domain from dropdown
      // Manual entry allows wildcards directly (e.g., *.example.com)
      if (data.domainInputType === 'select' && data.domain && data.domain.includes('*.')) {
        return data.subdomain && data.subdomain.trim().length > 0
      }
      return true
    },
    {
      message: 'Subdomain is required when selecting a wildcard domain',
      path: ['subdomain'],
    }
  )

type AddRouteFormData = z.infer<typeof addRouteSchema>

export function AddRoute() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const navigate = useNavigate()
  const { accessMode } = usePlatformCapabilities()
  const isLocalMode = useMemo(() => accessMode === 'local', [accessMode])

  const { data: domainsData } = useQuery({
    ...listDomainsOptions(),
  })

  const hasAvailableDomains = useMemo(
    () => domainsData?.domains && domainsData.domains.length > 0,
    [domainsData]
  )

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Load Balancer', href: '/load-balancer' },
      { label: 'Add Route' },
    ])
  }, [setBreadcrumbs])

  usePageTitle('Add Route')

  const form = useForm<AddRouteFormData>({
    resolver: zodResolver(addRouteSchema),
    defaultValues: {
      routeType: 'http',
      domainInputType: isLocalMode || !hasAvailableDomains ? 'manual' : 'select',
      domain: '',
      subdomain: '',
      host: '',
      port: 80,
    },
  })

  // Update domainInputType when hasAvailableDomains or isLocalMode changes
  useEffect(() => {
    if (isLocalMode || !hasAvailableDomains) {
      form.setValue('domainInputType', 'manual')
    }
  }, [isLocalMode, hasAvailableDomains, form])

  const createRoute = useMutation({
    ...createRouteMutation(),
    meta: {
      errorTitle: 'Failed to create route',
    },
    onSuccess: () => {
      toast.success('Route created successfully!')
      navigate('/load-balancer')
    },
  })

  const watchedRouteType = useWatch({ control: form.control, name: 'routeType' })
  const watchedDomainInputType = useWatch({ control: form.control, name: 'domainInputType' })
  const watchedDomain = useWatch({ control: form.control, name: 'domain' })

  // Only show subdomain field when selecting a wildcard domain from the dropdown
  // When manually entering, users can type the full domain including wildcards
  const showSubdomainField = useMemo(
    () => watchedDomainInputType === 'select' && watchedDomain && watchedDomain.includes('*.'),
    [watchedDomainInputType, watchedDomain]
  )

  const handleSubmit = async (data: AddRouteFormData) => {
    let finalDomain = data.domain

    // If selecting a wildcard domain from dropdown, construct the final domain with subdomain
    if (showSubdomainField) {
      if (!data.subdomain || data.subdomain.trim() === '') {
        toast.error('Subdomain is required when selecting a wildcard domain')
        return
      }
      finalDomain = data.domain.replace('*.', `${data.subdomain}.`)
    }

    await createRoute.mutateAsync({
      body: {
        domain: finalDomain,
        host: data.host,
        port: data.port,
        route_type: data.routeType,
      },
    })
  }

  return (
    <div className="flex-1 overflow-auto">
      <div className="max-w-4xl mx-auto space-y-6">
        {/* Header */}
        <div className="flex items-center gap-4">
          <Button
            variant="ghost"
            size="icon"
            onClick={() => navigate('/load-balancer')}
          >
            <ArrowLeft className="h-4 w-4" />
          </Button>
          <div>
            <h1 className="text-2xl font-bold">Add New Route</h1>
            <p className="text-sm text-muted-foreground mt-1">
              Configure routing rules to direct traffic to your backend services
            </p>
          </div>
        </div>

        <Form {...form}>
          <form onSubmit={form.handleSubmit(handleSubmit)} className="space-y-6">
            {/* Section 1: Routing Type */}
            <Card>
              <div className="p-6 space-y-4">
                <div className="flex items-center gap-2">
                  <Router className="h-5 w-5 text-primary" />
                  <h2 className="text-lg font-semibold">Routing Type</h2>
                </div>
                <p className="text-sm text-muted-foreground">
                  Choose how incoming traffic should be matched and routed to your backend.
                </p>

                <FormField
                  control={form.control}
                  name="routeType"
                  render={({ field }) => (
                    <FormItem>
                      <FormControl>
                        <RadioGroup
                          onValueChange={field.onChange}
                          value={field.value}
                          className="grid gap-4 md:grid-cols-2"
                        >
                          {/* HTTP Routing Card */}
                          <Card
                            className={`relative cursor-pointer border-2 transition-colors ${
                              field.value === 'http'
                                ? 'border-primary bg-primary/5'
                                : 'border-border hover:border-primary/50'
                            }`}
                          >
                            <label className="flex cursor-pointer flex-col p-4">
                              <div className="flex items-start gap-3">
                                <RadioGroupItem
                                  value="http"
                                  id="http"
                                  className="mt-1"
                                />
                                <div className="flex-1 space-y-2">
                                  <div className="flex items-center gap-2">
                                    <Globe className="h-5 w-5 text-blue-500" />
                                    <span className="font-semibold">HTTP Routing</span>
                                    <span className="text-xs bg-blue-100 text-blue-700 dark:bg-blue-900 dark:text-blue-300 px-2 py-0.5 rounded">
                                      Recommended
                                    </span>
                                  </div>
                                  <p className="text-sm text-muted-foreground">
                                    Routes traffic based on the HTTP Host header. Works for
                                    both HTTP and HTTPS after TLS termination.
                                  </p>
                                  <div className="space-y-1 text-sm pt-2">
                                    <div className="flex items-center gap-2 text-muted-foreground">
                                      <Layers className="h-4 w-4" />
                                      <span>Layer 7 (Application Layer)</span>
                                    </div>
                                    <div className="flex items-center gap-2 text-muted-foreground">
                                      <Server className="h-4 w-4" />
                                      <span>Full HTTP inspection & modification</span>
                                    </div>
                                    <div className="flex items-center gap-2 text-muted-foreground">
                                      <Lock className="h-4 w-4" />
                                      <span>SSL termination at proxy</span>
                                    </div>
                                  </div>
                                </div>
                              </div>
                            </label>
                          </Card>

                          {/* TLS/SNI Routing Card */}
                          <Card
                            className={`relative cursor-pointer border-2 transition-colors ${
                              field.value === 'tls'
                                ? 'border-primary bg-primary/5'
                                : 'border-border hover:border-primary/50'
                            }`}
                          >
                            <label className="flex cursor-pointer flex-col p-4">
                              <div className="flex items-start gap-3">
                                <RadioGroupItem
                                  value="tls"
                                  id="tls"
                                  className="mt-1"
                                />
                                <div className="flex-1 space-y-2">
                                  <div className="flex items-center gap-2">
                                    <Lock className="h-5 w-5 text-green-500" />
                                    <span className="font-semibold">TLS/SNI Routing</span>
                                    <span className="text-xs bg-green-100 text-green-700 dark:bg-green-900 dark:text-green-300 px-2 py-0.5 rounded">
                                      Passthrough
                                    </span>
                                  </div>
                                  <p className="text-sm text-muted-foreground">
                                    Routes traffic based on TLS SNI (Server Name Indication)
                                    hostname. Traffic is passed through without TLS termination.
                                  </p>
                                  <div className="space-y-1 text-sm pt-2">
                                    <div className="flex items-center gap-2 text-muted-foreground">
                                      <Layers className="h-4 w-4" />
                                      <span>Layer 4/5 (Transport Layer)</span>
                                    </div>
                                    <div className="flex items-center gap-2 text-muted-foreground">
                                      <Server className="h-4 w-4" />
                                      <span>TCP passthrough (no inspection)</span>
                                    </div>
                                    <div className="flex items-center gap-2 text-muted-foreground">
                                      <Lock className="h-4 w-4" />
                                      <span>End-to-end encryption preserved</span>
                                    </div>
                                  </div>
                                </div>
                              </div>
                            </label>
                          </Card>
                        </RadioGroup>
                      </FormControl>
                      <FormMessage />
                    </FormItem>
                  )}
                />

                {watchedRouteType === 'tls' && (
                  <Alert>
                    <Info className="h-4 w-4" />
                    <AlertDescription>
                      <strong>TLS/SNI routing</strong> passes encrypted traffic directly to
                      your backend without terminating TLS. Your backend service must handle
                      its own SSL certificates. This is useful for services that require
                      end-to-end encryption or have their own certificate management.
                    </AlertDescription>
                  </Alert>
                )}
              </div>
            </Card>

            {/* Section 2: Domain Configuration */}
            <Card>
              <div className="p-6 space-y-4">
                <div className="flex items-center gap-2">
                  <Globe className="h-5 w-5 text-primary" />
                  <h2 className="text-lg font-semibold">Domain Configuration</h2>
                </div>
                <p className="text-sm text-muted-foreground">
                  Specify the domain that will be routed to your backend service.
                </p>

                {/* Domain Input Type Selection */}
                {hasAvailableDomains && !isLocalMode && (
                  <FormField
                    control={form.control}
                    name="domainInputType"
                    render={({ field }) => (
                      <FormItem className="space-y-3">
                        <FormLabel>Domain Source</FormLabel>
                        <FormControl>
                          <RadioGroup
                            onValueChange={(value) => {
                              field.onChange(value)
                              form.setValue('domain', '')
                              form.setValue('subdomain', '')
                            }}
                            value={field.value}
                            className="flex gap-4"
                          >
                            <div className="flex items-center space-x-2">
                              <RadioGroupItem value="select" id="domain-select" />
                              <Label htmlFor="domain-select">
                                Select from managed domains
                              </Label>
                            </div>
                            <div className="flex items-center space-x-2">
                              <RadioGroupItem value="manual" id="domain-manual" />
                              <Label htmlFor="domain-manual">Enter manually</Label>
                            </div>
                          </RadioGroup>
                        </FormControl>
                        <FormMessage />
                      </FormItem>
                    )}
                  />
                )}

                {/* Domain Input */}
                <FormField
                  control={form.control}
                  name="domain"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Domain</FormLabel>
                      <FormControl>
                        {watchedDomainInputType === 'select' &&
                        hasAvailableDomains &&
                        !isLocalMode ? (
                          <Select onValueChange={field.onChange} value={field.value}>
                            <SelectTrigger>
                              <SelectValue placeholder="Select a domain" />
                            </SelectTrigger>
                            <SelectContent>
                              {domainsData?.domains?.map((domain) => (
                                <SelectItem key={domain.id} value={domain.domain}>
                                  {domain.domain}
                                </SelectItem>
                              ))}
                            </SelectContent>
                          </Select>
                        ) : (
                          <Input
                            {...field}
                            placeholder="example.com or app.example.com"
                          />
                        )}
                      </FormControl>
                      <FormDescription>
                        {isLocalMode ? (
                          'Manual entry required in local development mode'
                        ) : !hasAvailableDomains ? (
                          'No managed domains available - enter domain manually'
                        ) : watchedDomainInputType === 'select' ? (
                          'Choose from your managed domains with SSL certificates'
                        ) : (
                          'Enter the full domain name for this route'
                        )}
                      </FormDescription>
                      <FormMessage />
                    </FormItem>
                  )}
                />

                {/* Subdomain for wildcard domains selected from dropdown */}
                {showSubdomainField && (
                  <FormField
                    control={form.control}
                    name="subdomain"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Subdomain</FormLabel>
                        <FormControl>
                          <div className="flex items-center gap-2">
                            <Input
                              {...field}
                              placeholder="subdomain"
                              className="max-w-[200px]"
                            />
                            <span className="text-sm text-muted-foreground">
                              {watchedDomain.replace('*', '')}
                            </span>
                          </div>
                        </FormControl>
                        <FormDescription>
                          Specify the subdomain for this wildcard domain route
                        </FormDescription>
                        <FormMessage />
                      </FormItem>
                    )}
                  />
                )}
              </div>
            </Card>

            {/* Section 3: Backend Configuration */}
            <Card>
              <div className="p-6 space-y-4">
                <div className="flex items-center gap-2">
                  <Server className="h-5 w-5 text-primary" />
                  <h2 className="text-lg font-semibold">Backend Configuration</h2>
                </div>
                <p className="text-sm text-muted-foreground">
                  Configure where traffic should be forwarded to.
                </p>

                <div className="grid gap-4 md:grid-cols-2">
                  <FormField
                    control={form.control}
                    name="host"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Host</FormLabel>
                        <FormControl>
                          <Input
                            {...field}
                            placeholder="localhost or 192.168.1.100"
                          />
                        </FormControl>
                        <FormDescription>
                          IP address or hostname of your backend service
                        </FormDescription>
                        <FormMessage />
                      </FormItem>
                    )}
                  />

                  <FormField
                    control={form.control}
                    name="port"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Port</FormLabel>
                        <FormControl>
                          <Input
                            type="number"
                            min={1}
                            max={65535}
                            placeholder="8080"
                            value={field.value}
                            onChange={(e) => field.onChange(Number(e.target.value) || 0)}
                          />
                        </FormControl>
                        <FormDescription>
                          Port number your service is listening on
                        </FormDescription>
                        <FormMessage />
                      </FormItem>
                    )}
                  />
                </div>

                <Alert>
                  <Info className="h-4 w-4" />
                  <AlertDescription>
                    <div className="space-y-2">
                      <p>
                        Traffic to <strong>{watchedDomain || 'your domain'}</strong> will
                        be forwarded to{' '}
                        <strong>
                          {form.watch('host') || 'host'}:{form.watch('port') || 'port'}
                        </strong>
                      </p>
                      {watchedRouteType === 'http' && (
                        <p className="text-xs">
                          Using HTTP routing: TLS will be terminated at the proxy and
                          traffic forwarded to your backend over HTTP.
                        </p>
                      )}
                      {watchedRouteType === 'tls' && (
                        <p className="text-xs">
                          Using TLS passthrough: Encrypted traffic will be forwarded
                          directly to your backend. Ensure your backend handles TLS.
                        </p>
                      )}
                    </div>
                  </AlertDescription>
                </Alert>
              </div>
            </Card>

            {/* Actions */}
            <div className="flex justify-between gap-4">
              <Button
                type="button"
                variant="outline"
                onClick={() => navigate('/load-balancer')}
              >
                Cancel
              </Button>
              <Button type="submit" disabled={createRoute.isPending}>
                {createRoute.isPending ? (
                  'Creating Route...'
                ) : (
                  <>
                    Create Route
                    <ArrowRight className="ml-2 h-4 w-4" />
                  </>
                )}
              </Button>
            </div>
          </form>
        </Form>
      </div>
    </div>
  )
}
