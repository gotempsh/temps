import {
  cancelDomainOrderMutation,
  createOrRecreateOrderMutation,
  finalizeOrderMutation,
  getDomainByIdOptions,
  getDomainOrderOptions,
  getHttpChallengeDebugOptions,
  getPublicIpOptions,
  listProvidersOptions,
  renewDomainMutation,
  setupDnsChallengeMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Separator } from '@/components/ui/separator'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { usePlatformCapabilities } from '@/hooks/usePlatformCapabilities'
import { formatUTCDate } from '@/lib/date'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  AlertTriangle,
  ArrowLeft,
  Calendar,
  CheckCircle,
  Clock,
  Copy,
  CopyCheck,
  ExternalLink,
  Globe,
  Info,
  Loader2,
  RefreshCw,
  Shield,
  Wand2,
  XCircle,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { useNavigate, useParams } from 'react-router-dom'
import { toast } from 'sonner'

export function DomainDetail() {
  const { id } = useParams<{ id: string }>()
  const { setBreadcrumbs } = useBreadcrumbs()
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const [copiedField, setCopiedField] = useState<string | null>(null)
  const [selectedDnsProvider, setSelectedDnsProvider] = useState<string>('')

  const {
    data: domain,
    isLoading: isDomainLoading,
    refetch: refetchDomain,
  } = useQuery({
    ...getDomainByIdOptions({
      path: {
        domain: Number(id),
      },
    }),
    enabled: !!id,
  })

  const {
    data: order,
    isLoading: isOrderLoading,
    refetch: refetchOrder,
  } = useQuery({
    ...getDomainOrderOptions({
      path: {
        domain_id: Number(id),
      },
    }),
    enabled: !!id && domain?.status !== 'active',
    retry: false,
  })

  const { data: httpDebugInfo } = useQuery({
    ...getHttpChallengeDebugOptions({
      path: {
        domain: domain?.domain || '',
      },
    }),
    enabled:
      !!domain?.domain &&
      domain?.verification_method === 'http-01' &&
      (domain?.status === 'challenge_requested' ||
        domain?.status === 'pending' ||
        domain?.status === 'pending_http'),
    retry: false,
  })

  const { data: publicIpData } = useQuery({
    ...getPublicIpOptions(),
    enabled:
      !!domain &&
      domain?.verification_method === 'http-01' &&
      (domain?.status === 'challenge_requested' ||
        domain?.status === 'pending' ||
        domain?.status === 'pending_http'),
  })

  // Fetch DNS providers for auto-provisioning when DNS-01 challenge is available
  const { data: dnsProviders } = useQuery({
    ...listProvidersOptions(),
    enabled:
      !!domain &&
      domain?.verification_method === 'dns-01' &&
      (domain?.status === 'challenge_requested' ||
        domain?.status === 'pending_dns' ||
        domain?.status === 'pending'),
  })

  const { canManageCertificates, isUsingCloudflare } = usePlatformCapabilities()

  useEffect(() => {
    if (domain) {
      setBreadcrumbs([
        { label: 'Domains', href: '/domains' },
        { label: domain.domain },
      ])
    }
  }, [setBreadcrumbs, domain])

  usePageTitle(domain ? `${domain.domain} - Domain Details` : 'Domain Details')

  const createOrder = useMutation({
    ...createOrRecreateOrderMutation(),
    meta: {
      errorTitle: 'Failed to create ACME order',
    },
    onSuccess: () => {
      toast.success('ACME order created successfully')
      refetchDomain()
      refetchOrder()
    },
  })

  const finalizeOrder = useMutation({
    ...finalizeOrderMutation(),
    meta: {
      errorTitle: 'Failed to verify DNS challenge',
    },
    onSuccess: async () => {
      toast.success(
        'DNS challenge verified! SSL certificate provisioning in progress.'
      )

      // Initial refetch
      await refetchDomain()
      await refetchOrder()

      // Poll for status updates until domain becomes active
      const pollInterval = setInterval(async () => {
        const result = await refetchDomain()
        if (result.data?.status === 'active') {
          clearInterval(pollInterval)
          toast.success('SSL certificate is now active!')
          await refetchDomain()
          await refetchOrder()
        }
      }, 3000) // Check every 3 seconds

      // Stop polling after 2 minutes
      setTimeout(() => clearInterval(pollInterval), 120000)
    },
  })

  const cancelOrder = useMutation({
    ...cancelDomainOrderMutation(),
    meta: {
      errorTitle: 'Failed to cancel ACME order',
    },
    onSuccess: async () => {
      toast.success('ACME order cancelled. You can now start over.')

      // Invalidate and clear the order query
      queryClient.setQueryData(
        getDomainOrderOptions({
          path: { domain_id: Number(id) },
        }).queryKey,
        undefined
      )

      await refetchDomain()
      await refetchOrder()
    },
  })

  const renewDomain = useMutation({
    ...renewDomainMutation(),
    meta: {
      errorTitle: 'Failed to renew certificate',
    },
    onSuccess: (data) => {
      // Check if the response is a challenge (DNS-01 renewal)
      // The API returns 202 with challenge data for DNS-01 domains
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const response = data as any
      if (response?.txt_records && response.txt_records.length > 0) {
        // DNS-01 challenge was created - user needs to update DNS and finalize
        toast.success(
          'Renewal order created. Update DNS TXT records and click "Verify & Complete" to finalize.',
          { duration: 6000 }
        )
      } else if (response?.Complete) {
        // HTTP-01 certificate was renewed successfully
        toast.success('Certificate renewed successfully')
      } else {
        // Generic success
        toast.success('Certificate renewal initiated')
      }
      refetchDomain()
      refetchOrder()
    },
  })

  const setupDns = useMutation({
    ...setupDnsChallengeMutation(),
    meta: {
      errorTitle: 'Failed to setup DNS records',
    },
    onSuccess: (data) => {
      if (data.success) {
        toast.success(data.message)
      } else {
        toast.warning(data.message)
      }
      // Show individual record results
      data.results.forEach((result) => {
        if (!result.success) {
          toast.error(`${result.name}: ${result.message}`)
        }
      })
    },
  })

  const handleCopy = (text: string, field: string) => {
    navigator.clipboard.writeText(text)
    setCopiedField(field)
    toast.success('Copied to clipboard')
    setTimeout(() => setCopiedField(null), 2000)
  }

  const handleCreateOrder = () => {
    if (!domain) return
    createOrder.mutate({
      path: {
        domain_id: domain.id,
      },
    })
  }

  const handleCompleteDns = () => {
    if (!domain) return
    // Finalize the order after DNS challenge verification
    finalizeOrder.mutate({
      path: {
        domain_id: domain.id,
      },
    })
  }

  const handleSetupDnsRecords = async () => {
    if (!domain || !selectedDnsProvider) return
    await setupDns.mutateAsync({
      path: {
        domain_id: domain.id,
      },
      body: {
        dns_provider_id: parseInt(selectedDnsProvider, 10),
      },
    })
  }

  const handleCancelOrder = () => {
    if (!domain) return
    toast.promise(
      cancelOrder.mutateAsync({
        path: {
          domain_id: domain.id,
        },
      }),
      {
        loading: 'Cancelling ACME order...',
        success: 'ACME order cancelled successfully',
        error: 'Failed to cancel ACME order',
      }
    )
  }

  const handleRenewDomain = async () => {
    if (!domain) return
    const loadingToast = toast.loading(
      domain.verification_method === 'dns-01'
        ? 'Creating renewal order...'
        : 'Renewing certificate...'
    )
    try {
      await renewDomain.mutateAsync({
        path: {
          domain: domain.domain,
        },
      })
    } catch {
      // Error is handled by the mutation's meta.errorTitle
    } finally {
      toast.dismiss(loadingToast)
    }
  }

  const isExpiringSoon = (expirationTime?: number | null) => {
    if (!expirationTime) return false
    const expirationDate = new Date(expirationTime)
    const now = new Date()
    const daysUntilExpiration = Math.ceil(
      (expirationDate.getTime() - now.getTime()) / (1000 * 60 * 60 * 24)
    )
    return daysUntilExpiration <= 15
  }

  const getStatusBadgeVariant = (status: string) => {
    switch (status) {
      case 'active':
      case 'valid':
        return 'default'
      case 'pending':
      case 'processing':
        return 'secondary'
      case 'failed':
      case 'invalid':
        return 'destructive'
      default:
        return 'outline'
    }
  }

  const isLoading = isDomainLoading || isOrderLoading

  if (isLoading && !domain) {
    return (
      <div className="flex-1 overflow-auto">
        <div className="flex items-center justify-center min-h-[400px]">
          <Loader2 className="h-8 w-8 animate-spin text-muted-foreground" />
        </div>
      </div>
    )
  }

  if (!domain) {
    return (
      <div className="flex-1 overflow-auto">
        <div className="max-w-5xl mx-auto p-6">
          <Alert variant="warning">
            <AlertTriangle className="h-4 w-4" />
            <AlertTitle>Domain not found</AlertTitle>
            <AlertDescription>
              The requested domain could not be found.
            </AlertDescription>
          </Alert>
          <Button className="mt-4" onClick={() => navigate('/domains')}>
            <ArrowLeft className="mr-2 h-4 w-4" />
            Back to Domains
          </Button>
        </div>
      </div>
    )
  }

  // Get challenge info from order (works for both DNS and HTTP challenges)
  const challengeData = order?.authorizations as
    | {
        challenge_type: 'dns-01' | 'http-01'
        dns_txt_records: {
          name: string
          value: string
        }[]
        key_authorization: string
        token: string
        validation_url: string
      }
    | undefined

  // DNS-01 specific
  const hasDnsChallenge = order && domain.verification_method === 'dns-01'
  const dnsTxtRecords = challengeData?.dns_txt_records || []
  const hasDnsValues = dnsTxtRecords.length > 0

  // HTTP-01 specific
  const hasHttpChallenge =
    order && domain.verification_method === 'http-01' && challengeData
  return (
    <div className="flex-1 overflow-auto">
      <div className="max-w-7xl mx-auto space-y-6">
        {/* Header */}
        <div className="flex items-center justify-between gap-4">
          <div className="flex items-center gap-4">
            <Button
              variant="ghost"
              size="icon"
              onClick={() => navigate('/domains')}
            >
              <ArrowLeft className="h-4 w-4" />
            </Button>
            <div>
              <div className="flex items-center gap-3">
                <h1 className="text-2xl font-bold">{domain.domain}</h1>
                <Badge variant={getStatusBadgeVariant(domain.status)}>
                  {domain.status}
                </Badge>
                {domain.is_wildcard && (
                  <Badge variant="outline">Wildcard</Badge>
                )}
              </div>
              <p className="text-sm text-muted-foreground mt-1">
                SSL Certificate & Order Management
              </p>
            </div>
          </div>
          <div className="flex gap-2">
            {(domain.status === 'pending_dns' || domain.status === 'pending') &&
              !order && (
                <Button
                  variant="default"
                  size="sm"
                  onClick={handleCreateOrder}
                  disabled={!canManageCertificates || createOrder.isPending}
                >
                  {createOrder.isPending ? (
                    <>
                      <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                      Creating Order...
                    </>
                  ) : (
                    <>
                      <Shield className="mr-2 h-4 w-4" />
                      Create Order
                    </>
                  )}
                </Button>
              )}
            {(domain.status === 'pending_dns' || domain.status === 'pending') &&
              order &&
              hasDnsChallenge && (
                <Button
                  variant="default"
                  size="sm"
                  onClick={handleCompleteDns}
                  disabled={!canManageCertificates || finalizeOrder.isPending}
                >
                  {finalizeOrder.isPending ? (
                    <>
                      <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                      Verifying...
                    </>
                  ) : (
                    <>
                      <CheckCircle className="mr-2 h-4 w-4" />
                      Verify & Finalize
                    </>
                  )}
                </Button>
              )}
            <Button
              variant="outline"
              size="sm"
              onClick={() => {
                refetchDomain()
                refetchOrder()
              }}
            >
              <RefreshCw className="mr-2 h-4 w-4" />
              Refresh
            </Button>
          </div>
        </div>

        {/* Cloudflare mode information */}
        {isUsingCloudflare() && (
          <Alert className="border-purple-200 bg-purple-50/50 dark:bg-purple-950/10">
            <Info className="h-4 w-4 text-purple-600" />
            <AlertDescription>
              Domain and certificate management is handled automatically by
              Cloudflare Tunnel.
            </AlertDescription>
          </Alert>
        )}

        {/* Expiring Soon Alert */}
        {domain.status === 'active' &&
          isExpiringSoon(domain.expiration_time) && (
            <Alert variant="warning">
              <AlertTriangle className="h-4 w-4" />
              <AlertTitle>Certificate Expiring Soon</AlertTitle>
              <AlertDescription className="flex items-center justify-between">
                <span>
                  The SSL certificate will expire on{' '}
                  {formatUTCDate(domain.expiration_time || 0)}. Renew it before
                  expiration to avoid service interruption.
                </span>
                <Button
                  size="sm"
                  onClick={handleRenewDomain}
                  disabled={!canManageCertificates || renewDomain.isPending}
                >
                  <RefreshCw className="mr-2 h-4 w-4" />
                  {canManageCertificates
                    ? 'Renew Now'
                    : 'Managed by Cloudflare'}
                </Button>
              </AlertDescription>
            </Alert>
          )}

        {/* Error Alert */}
        {domain.last_error && (
          <Alert variant="warning">
            <AlertTriangle className="h-4 w-4" />
            <AlertTitle>
              Error: {domain.last_error_type || 'Certificate Error'}
            </AlertTitle>
            <AlertDescription>{domain.last_error}</AlertDescription>
          </Alert>
        )}

        <div className="grid grid-cols-1 lg:grid-cols-3 gap-6">
          {/* Main Content */}
          <div className="lg:col-span-2 space-y-6">
            <Card>
              <div className="p-6 space-y-4">
                {/* Active Certificate */}
                {domain.status === 'active' && (
                  <>
                    <div className="flex items-center justify-between">
                      <div className="flex items-center gap-2">
                        <CheckCircle className="h-5 w-5 text-green-600" />
                        <h2 className="text-lg font-semibold">
                          Active SSL Certificate
                        </h2>
                      </div>
                      <Button
                        onClick={handleRenewDomain}
                        variant="outline"
                        size="sm"
                        disabled={
                          !canManageCertificates || renewDomain.isPending
                        }
                      >
                        <RefreshCw className="mr-2 h-4 w-4" />
                        {canManageCertificates
                          ? 'Renew Certificate'
                          : 'Managed by Cloudflare'}
                      </Button>
                    </div>

                    <Alert className="border-green-200 bg-green-50/50 dark:bg-green-950/10">
                      <CheckCircle className="h-4 w-4 text-green-600" />
                      <AlertDescription>
                        Your SSL certificate is active and your domain is
                        secured with HTTPS.
                      </AlertDescription>
                    </Alert>

                    {/* Certificate Validity */}
                    <div className="grid grid-cols-1 md:grid-cols-2 gap-4 p-4 bg-muted/50 rounded-lg">
                      {domain.last_renewed ? (
                        <div className="space-y-1">
                          <span className="text-xs font-medium text-muted-foreground">
                            Last Renewed
                          </span>
                          <p className="text-sm font-medium flex items-center gap-2">
                            <Clock className="h-4 w-4" />
                            {formatUTCDate(domain.last_renewed)}
                          </p>
                        </div>
                      ) : null}
                      {domain.expiration_time ? (
                        <div className="space-y-1">
                          <span className="text-xs font-medium text-muted-foreground">
                            Expires
                          </span>
                          <p className="text-sm font-medium flex items-center gap-2">
                            <Calendar className="h-4 w-4" />
                            {formatUTCDate(domain.expiration_time || 0)}
                          </p>
                        </div>
                      ) : null}
                    </div>

                    {/* Certificate PEM */}
                    {domain.certificate && (
                      <>
                        <div className="space-y-2">
                          <div className="flex items-center justify-between">
                            <span className="text-sm font-medium">
                              Certificate (PEM Format)
                            </span>
                            <Button
                              size="sm"
                              variant="ghost"
                              className="h-8 w-8 p-0"
                              onClick={() =>
                                handleCopy(
                                  domain.certificate || '',
                                  'certificate'
                                )
                              }
                            >
                              {copiedField === 'certificate' ? (
                                <CopyCheck className="h-4 w-4" />
                              ) : (
                                <Copy className="h-4 w-4" />
                              )}
                            </Button>
                          </div>
                          <div className="relative">
                            <pre className="p-4 bg-muted rounded-lg text-xs font-mono overflow-x-auto max-h-48 overflow-y-auto">
                              {domain.certificate}
                            </pre>
                          </div>
                        </div>
                      </>
                    )}
                  </>
                )}

                {/* DNS Challenge Instructions */}
                {(domain.status === 'challenge_requested' ||
                  domain.status === 'pending_dns' ||
                  domain.status === 'pending_http' ||
                  domain.status === 'pending') &&
                  domain.verification_method === 'dns-01' && (
                    <>
                      <div className="flex items-center justify-between">
                        <h2 className="text-lg font-semibold">
                          DNS Challenge Required
                        </h2>
                        <Badge variant={getStatusBadgeVariant(domain.status)}>
                          {domain.status}
                        </Badge>
                      </div>

                      {!order && (
                        <Alert>
                          <Info className="h-4 w-4" />
                          <AlertTitle>Create ACME Order</AlertTitle>
                          <AlertDescription>
                            <p className="mb-4">
                              Create an ACME order to get your DNS challenge
                              token.
                            </p>
                            <Button
                              onClick={handleCreateOrder}
                              disabled={
                                !canManageCertificates || createOrder.isPending
                              }
                            >
                              {createOrder.isPending ? (
                                <>
                                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                                  Creating...
                                </>
                              ) : (
                                <>
                                  <Shield className="mr-2 h-4 w-4" />
                                  Create Order
                                </>
                              )}
                            </Button>
                          </AlertDescription>
                        </Alert>
                      )}

                      {order && hasDnsValues && (
                        <>
                          <Alert>
                            <Info className="h-4 w-4" />
                            <AlertTitle>Step 1: Add DNS TXT Record</AlertTitle>
                            <AlertDescription>
                              Add the following TXT record
                              {dnsTxtRecords.length > 1 ? 's' : ''} to your DNS
                              provider:
                            </AlertDescription>
                          </Alert>

                          <div className="space-y-4">
                            {dnsTxtRecords.map((record, index) => (
                              <div
                                key={index}
                                className="space-y-3 p-4 bg-muted/50 rounded-lg"
                              >
                                {dnsTxtRecords.length > 1 && (
                                  <div className="flex items-center justify-between">
                                    <span className="text-sm font-medium">
                                      Record {index + 1}
                                    </span>
                                    <Badge variant="outline">TXT</Badge>
                                  </div>
                                )}
                                <div className="flex items-center justify-between gap-2">
                                  <div className="space-y-1 flex-1 min-w-0">
                                    <span className="text-xs font-medium text-muted-foreground">
                                      Name
                                    </span>
                                    <p className="font-mono text-sm break-all">
                                      {record.name}
                                    </p>
                                  </div>
                                  <Button
                                    size="sm"
                                    variant="outline"
                                    onClick={() =>
                                      handleCopy(
                                        record.name,
                                        `main-name-${index}`
                                      )
                                    }
                                  >
                                    {copiedField === `main-name-${index}` ? (
                                      <CopyCheck className="h-4 w-4" />
                                    ) : (
                                      <Copy className="h-4 w-4" />
                                    )}
                                  </Button>
                                </div>
                                <div className="flex items-center justify-between gap-2">
                                  <div className="space-y-1 flex-1 min-w-0">
                                    <span className="text-xs font-medium text-muted-foreground">
                                      Value
                                    </span>
                                    <p className="font-mono text-sm break-all">
                                      {record.value}
                                    </p>
                                  </div>
                                  <Button
                                    size="sm"
                                    variant="outline"
                                    onClick={() =>
                                      handleCopy(
                                        record.value,
                                        `main-value-${index}`
                                      )
                                    }
                                  >
                                    {copiedField === `main-value-${index}` ? (
                                      <CopyCheck className="h-4 w-4" />
                                    ) : (
                                      <Copy className="h-4 w-4" />
                                    )}
                                  </Button>
                                </div>
                              </div>
                            ))}
                          </div>

                          {/* DNS Auto-Provisioning Option */}
                          {dnsProviders && dnsProviders.length > 0 && (
                            <div className="p-4 bg-muted/50 rounded-lg border">
                              <div className="flex items-start gap-3">
                                <div className="p-2 bg-primary/10 rounded-lg">
                                  <Wand2 className="h-5 w-5 text-primary" />
                                </div>
                                <div className="flex-1 space-y-3">
                                  <div>
                                    <h4 className="font-medium">
                                      Auto-Provision DNS Records
                                    </h4>
                                    <p className="text-sm text-muted-foreground">
                                      Automatically create the required TXT
                                      records using one of your configured DNS
                                      providers.
                                    </p>
                                  </div>
                                  <div className="flex flex-col sm:flex-row gap-2">
                                    <Select
                                      value={selectedDnsProvider}
                                      onValueChange={setSelectedDnsProvider}
                                    >
                                      <SelectTrigger className="w-full sm:w-[220px]">
                                        <SelectValue placeholder="Select DNS provider" />
                                      </SelectTrigger>
                                      <SelectContent>
                                        {dnsProviders.map((provider) => (
                                          <SelectItem
                                            key={provider.id}
                                            value={provider.id.toString()}
                                          >
                                            {provider.name} (
                                            {provider.provider_type})
                                          </SelectItem>
                                        ))}
                                      </SelectContent>
                                    </Select>
                                    <Button
                                      onClick={handleSetupDnsRecords}
                                      disabled={
                                        !selectedDnsProvider ||
                                        setupDns.isPending ||
                                        !canManageCertificates
                                      }
                                    >
                                      {setupDns.isPending ? (
                                        <>
                                          <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                                          Creating Records...
                                        </>
                                      ) : (
                                        <>
                                          <Wand2 className="mr-2 h-4 w-4" />
                                          Auto-Create Records
                                        </>
                                      )}
                                    </Button>
                                  </div>
                                  {!dnsProviders.length && (
                                    <p className="text-xs text-muted-foreground">
                                      <a
                                        href="/dns-providers/add"
                                        className="underline"
                                      >
                                        Add a DNS provider
                                      </a>{' '}
                                      to enable automatic DNS record creation.
                                    </p>
                                  )}
                                </div>
                              </div>
                            </div>
                          )}

                          <Separator className="my-2" />

                          <p className="text-sm text-muted-foreground text-center">
                            — or add the records manually —
                          </p>

                          <Alert>
                            <Clock className="h-4 w-4" />
                            <AlertTitle>
                              Step 2: Wait for DNS Propagation
                            </AlertTitle>
                            <AlertDescription>
                              <p className="mb-2">
                                After adding the TXT record
                                {dnsTxtRecords.length > 1 ? 's' : ''}, wait for{' '}
                                {dnsTxtRecords.length > 1 ? 'them' : 'it'} to
                                propagate (usually 5-15 minutes, up to 24
                                hours).
                              </p>
                              <p className="text-sm">
                                Check propagation:
                                {dnsTxtRecords.map((record, index) => (
                                  <span key={index}>
                                    {index > 0 && ', '}
                                    <a
                                      href={`https://www.whatsmydns.net/#TXT/${record.name}`}
                                      target="_blank"
                                      rel="noopener noreferrer"
                                      className="underline inline-flex items-center gap-1"
                                    >
                                      {record.name}{' '}
                                      <ExternalLink className="h-3 w-3" />
                                    </a>
                                  </span>
                                ))}
                              </p>
                            </AlertDescription>
                          </Alert>

                          <Alert>
                            <CheckCircle className="h-4 w-4" />
                            <AlertTitle>Step 3: Verify & Complete</AlertTitle>
                            <AlertDescription>
                              <p className="mb-4">
                                Once the DNS record has propagated, click the
                                button below to verify and provision your SSL
                                certificate.
                              </p>
                              <div className="flex gap-2">
                                <Button
                                  onClick={handleCompleteDns}
                                  disabled={
                                    finalizeOrder.isPending ||
                                    !canManageCertificates
                                  }
                                >
                                  {finalizeOrder.isPending ? (
                                    <>
                                      <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                                      Verifying...
                                    </>
                                  ) : (
                                    <>
                                      <CheckCircle className="mr-2 h-4 w-4" />
                                      Verify & Finalize
                                    </>
                                  )}
                                </Button>
                                <Button
                                  variant="outline"
                                  onClick={handleCancelOrder}
                                  disabled={!canManageCertificates}
                                >
                                  <XCircle className="mr-2 h-4 w-4" />
                                  Cancel
                                </Button>
                              </div>
                            </AlertDescription>
                          </Alert>
                        </>
                      )}
                    </>
                  )}

                {/* HTTP Challenge */}
                {(domain.status === 'challenge_requested' ||
                  domain.status === 'pending' ||
                  domain.status === 'pending_http') &&
                  domain.verification_method === 'http-01' && (
                    <>
                      <div className="flex items-center justify-between">
                        <h2 className="text-lg font-semibold">
                          HTTP-01 Challenge
                        </h2>
                        <Badge variant={getStatusBadgeVariant(domain.status)}>
                          {domain.status}
                        </Badge>
                      </div>

                      <p className="text-sm text-muted-foreground">
                        Your SSL certificate is being provisioned using HTTP-01
                        validation. Ensure your domain&apos;s A record points to
                        your server IP and port 80 is accessible.
                      </p>

                      {/* DNS A Record Instructions */}
                      {publicIpData &&
                        typeof publicIpData === 'object' &&
                        'ip' in publicIpData &&
                        publicIpData.ip && (
                          <div className="space-y-2 p-4 bg-muted/50 rounded-lg border">
                            <div className="flex items-center justify-between">
                              <div className="flex items-center gap-2">
                                <Info className="h-4 w-4 text-blue-600" />
                                <span className="text-sm font-medium">
                                  DNS A Record Required
                                </span>
                              </div>
                              <a
                                href={`https://www.whatsmydns.net/#A/${domain.domain.replace('*.', '')}`}
                                target="_blank"
                                rel="noopener noreferrer"
                                className="text-xs text-blue-600 hover:underline inline-flex items-center gap-1"
                              >
                                Check DNS propagation
                                <ExternalLink className="h-3 w-3" />
                              </a>
                            </div>
                            <p className="text-xs text-muted-foreground">
                              Add this A record to your DNS provider:
                            </p>
                            <div className="grid grid-cols-[auto_1fr_auto] gap-2 items-center">
                              <div className="text-xs font-medium text-muted-foreground">
                                Name:
                              </div>
                              <code className="p-2 bg-background rounded text-xs font-mono">
                                {domain.is_wildcard
                                  ? `*.${domain.domain.replace('*.', '')}`
                                  : domain.domain}
                              </code>
                              <Button
                                size="sm"
                                variant="ghost"
                                className="h-8 w-8 p-0"
                                onClick={() =>
                                  handleCopy(
                                    domain.is_wildcard
                                      ? `*.${domain.domain.replace('*.', '')}`
                                      : domain.domain,
                                    'dns-name'
                                  )
                                }
                              >
                                {copiedField === 'dns-name' ? (
                                  <CopyCheck className="h-3 w-3" />
                                ) : (
                                  <Copy className="h-3 w-3" />
                                )}
                              </Button>

                              <div className="text-xs font-medium text-muted-foreground">
                                Type:
                              </div>
                              <code className="p-2 bg-background rounded text-xs font-mono">
                                A
                              </code>
                              <div />

                              <div className="text-xs font-medium text-muted-foreground">
                                Value:
                              </div>
                              <code className="p-2 bg-background rounded text-xs font-mono">
                                {publicIpData.ip as string}
                              </code>
                              <Button
                                size="sm"
                                variant="ghost"
                                className="h-8 w-8 p-0"
                                onClick={() =>
                                  handleCopy(
                                    publicIpData.ip as string,
                                    'dns-value'
                                  )
                                }
                              >
                                {copiedField === 'dns-value' ? (
                                  <CopyCheck className="h-3 w-3" />
                                ) : (
                                  <Copy className="h-3 w-3" />
                                )}
                              </Button>
                            </div>
                          </div>
                        )}

                      {/* Challenge URL and Expected Response */}
                      {hasHttpChallenge && challengeData && (
                        <div className="space-y-4">
                          <div className="space-y-2">
                            <div className="flex items-center justify-between">
                              <span className="text-sm font-medium">
                                Challenge URL
                              </span>
                              <div className="flex items-center gap-3">
                                <a
                                  href={`https://letsdebug.net`}
                                  target="_blank"
                                  rel="noopener noreferrer"
                                  className="text-xs text-blue-600 hover:underline inline-flex items-center gap-1"
                                >
                                  Let&apos;s Debug
                                  <ExternalLink className="h-3 w-3" />
                                </a>
                              </div>
                            </div>
                            <p className="text-xs text-muted-foreground">
                              Let&apos;s Encrypt will verify this URL:
                            </p>
                            <div className="flex items-center gap-2">
                              <code className="flex-1 p-3 bg-muted rounded text-xs font-mono break-all">
                                http://{domain.domain}
                                /.well-known/acme-challenge/
                                {challengeData.token}
                              </code>
                              <Button
                                size="sm"
                                variant="outline"
                                onClick={() =>
                                  handleCopy(
                                    `http://${domain.domain}/.well-known/acme-challenge/${challengeData.token}`,
                                    'challenge-url'
                                  )
                                }
                              >
                                {copiedField === 'challenge-url' ? (
                                  <CopyCheck className="h-4 w-4" />
                                ) : (
                                  <Copy className="h-4 w-4" />
                                )}
                              </Button>
                            </div>
                          </div>

                          <div className="space-y-2">
                            <span className="text-sm font-medium">
                              Expected Response
                            </span>
                            <p className="text-xs text-muted-foreground">
                              The URL should return this exact value:
                            </p>
                            <div className="flex items-center gap-2">
                              <code className="flex-1 p-3 bg-muted rounded text-xs font-mono break-all">
                                {challengeData.key_authorization}
                              </code>
                              <Button
                                size="sm"
                                variant="outline"
                                onClick={() =>
                                  handleCopy(
                                    challengeData.key_authorization,
                                    'expected-response'
                                  )
                                }
                              >
                                {copiedField === 'expected-response' ? (
                                  <CopyCheck className="h-4 w-4" />
                                ) : (
                                  <Copy className="h-4 w-4" />
                                )}
                              </Button>
                            </div>
                          </div>
                        </div>
                      )}

                      {/* Status Messages */}
                      {httpDebugInfo?.dns_error && (
                        <Alert variant="destructive">
                          <AlertTriangle className="h-4 w-4" />
                          <AlertTitle>DNS Configuration Issue</AlertTitle>
                          <AlertDescription>
                            <p>{httpDebugInfo.dns_error}</p>
                            <p className="text-sm mt-2">
                              Please ensure your domain&apos;s A record points
                              to your server IP address.
                            </p>
                          </AlertDescription>
                        </Alert>
                      )}

                      {!httpDebugInfo?.dns_error &&
                        httpDebugInfo &&
                        httpDebugInfo.dns_a_records.length === 0 && (
                          <Alert variant="warning">
                            <AlertTriangle className="h-4 w-4" />
                            <AlertTitle>No DNS Records Found</AlertTitle>
                            <AlertDescription>
                              <p>
                                Your domain doesn&apos;t have any A records
                                pointing to a server.
                              </p>
                              <p className="text-sm mt-2">
                                Add an A record in your DNS provider pointing to
                                your server&apos;s IP address.
                              </p>
                            </AlertDescription>
                          </Alert>
                        )}

                      {!httpDebugInfo?.dns_error &&
                        httpDebugInfo &&
                        httpDebugInfo.dns_a_records.length > 0 &&
                        httpDebugInfo.challenge_exists && (
                          <Alert className="border-green-200 bg-green-50/50 dark:bg-green-950/10">
                            <CheckCircle className="h-4 w-4 text-green-600" />
                            <AlertTitle>Ready for Validation</AlertTitle>
                            <AlertDescription>
                              Your domain is pointing to the server and the
                              challenge is ready for validation.
                            </AlertDescription>
                          </Alert>
                        )}

                      <div className="flex gap-2">
                        <Button
                          onClick={handleCompleteDns}
                          disabled={
                            finalizeOrder.isPending || !canManageCertificates
                          }
                        >
                          {finalizeOrder.isPending ? (
                            <>
                              <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                              Verifying...
                            </>
                          ) : (
                            <>
                              <CheckCircle className="mr-2 h-4 w-4" />
                              Verify & Provision SSL
                            </>
                          )}
                        </Button>
                        <Button
                          variant="outline"
                          onClick={handleCancelOrder}
                          disabled={!canManageCertificates}
                        >
                          <XCircle className="mr-2 h-4 w-4" />
                          Cancel Order
                        </Button>
                      </div>
                    </>
                  )}

                {/* Failed */}
                {domain.status === 'failed' && (
                  <>
                    <div className="flex items-center justify-between">
                      <h2 className="text-lg font-semibold">
                        DNS Challenge - Verification Failed
                      </h2>
                      <Badge variant="destructive">{domain.status}</Badge>
                    </div>

                    <Alert variant="destructive">
                      <AlertTriangle className="h-4 w-4" />
                      <AlertTitle>
                        Error: {domain.last_error_type || 'Validation Failed'}
                      </AlertTitle>
                      <AlertDescription>
                        {domain.last_error ||
                          'Certificate provisioning failed. Please verify your DNS records and try again.'}
                      </AlertDescription>
                    </Alert>

                    {domain.verification_method === 'dns-01' &&
                      order &&
                      hasDnsValues && (
                        <>
                          <Alert>
                            <Info className="h-4 w-4" />
                            <AlertTitle>DNS TXT Record Required</AlertTitle>
                            <AlertDescription>
                              Verify the following TXT record
                              {dnsTxtRecords.length > 1
                                ? 's exist'
                                : ' exists'}{' '}
                              in your DNS provider:
                            </AlertDescription>
                          </Alert>

                          <div className="space-y-4">
                            {dnsTxtRecords.map((record, index) => (
                              <div
                                key={index}
                                className="space-y-3 p-4 bg-muted/50 rounded-lg"
                              >
                                {dnsTxtRecords.length > 1 && (
                                  <div className="flex items-center justify-between">
                                    <span className="text-sm font-medium">
                                      Record {index + 1}
                                    </span>
                                    <Badge variant="outline">TXT</Badge>
                                  </div>
                                )}
                                <div className="flex items-center justify-between gap-2">
                                  <div className="space-y-1 flex-1 min-w-0">
                                    <span className="text-xs font-medium text-muted-foreground">
                                      Name
                                    </span>
                                    <p className="font-mono text-sm break-all">
                                      {record.name}
                                    </p>
                                  </div>
                                  <Button
                                    size="sm"
                                    variant="outline"
                                    onClick={() =>
                                      handleCopy(
                                        record.name,
                                        `failed-name-${index}`
                                      )
                                    }
                                  >
                                    {copiedField === `failed-name-${index}` ? (
                                      <CopyCheck className="h-4 w-4" />
                                    ) : (
                                      <Copy className="h-4 w-4" />
                                    )}
                                  </Button>
                                </div>
                                <div className="flex items-center justify-between gap-2">
                                  <div className="space-y-1 flex-1 min-w-0">
                                    <span className="text-xs font-medium text-muted-foreground">
                                      Value
                                    </span>
                                    <p className="font-mono text-sm break-all">
                                      {record.value}
                                    </p>
                                  </div>
                                  <Button
                                    size="sm"
                                    variant="outline"
                                    onClick={() =>
                                      handleCopy(
                                        record.value,
                                        `failed-value-${index}`
                                      )
                                    }
                                  >
                                    {copiedField === `failed-value-${index}` ? (
                                      <CopyCheck className="h-4 w-4" />
                                    ) : (
                                      <Copy className="h-4 w-4" />
                                    )}
                                  </Button>
                                </div>
                              </div>
                            ))}
                          </div>

                          <Alert>
                            <Clock className="h-4 w-4" />
                            <AlertTitle>Troubleshooting</AlertTitle>
                            <AlertDescription>
                              <ul className="list-disc list-inside space-y-1 text-sm">
                                <li>
                                  Verify the TXT record
                                  {dnsTxtRecords.length > 1
                                    ? 's are'
                                    : ' is'}{' '}
                                  correctly added to your DNS provider
                                </li>
                                <li>
                                  Check DNS propagation:
                                  {dnsTxtRecords.map((record, index) => (
                                    <span key={index}>
                                      {index > 0 && ', '}
                                      <a
                                        href={`https://www.whatsmydns.net/#TXT/${record.name}`}
                                        target="_blank"
                                        rel="noopener noreferrer"
                                        className="underline inline-flex items-center gap-1"
                                      >
                                        {record.name}{' '}
                                        <ExternalLink className="h-3 w-3" />
                                      </a>
                                    </span>
                                  ))}
                                </li>
                                <li>
                                  Wait for full DNS propagation (can take up to
                                  24 hours)
                                </li>
                                <li>
                                  Ensure there are no conflicting TXT records
                                </li>
                              </ul>
                            </AlertDescription>
                          </Alert>

                          <div className="flex gap-2">
                            <Button
                              onClick={handleCompleteDns}
                              disabled={
                                finalizeOrder.isPending ||
                                !canManageCertificates
                              }
                            >
                              {finalizeOrder.isPending ? (
                                <>
                                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                                  Retrying...
                                </>
                              ) : (
                                <>
                                  <RefreshCw className="mr-2 h-4 w-4" />
                                  Retry Verification
                                </>
                              )}
                            </Button>
                            <Button
                              variant="outline"
                              onClick={handleCancelOrder}
                              disabled={!canManageCertificates}
                            >
                              <XCircle className="mr-2 h-4 w-4" />
                              Cancel Order
                            </Button>
                          </div>
                        </>
                      )}
                  </>
                )}

                {/* Catch-all for other pending/challenge states */}
                {!domain.status.includes('active') &&
                  !(
                    (domain.status === 'challenge_requested' ||
                      domain.status === 'pending_dns' ||
                      domain.status === 'pending_http' ||
                      domain.status === 'pending') &&
                    domain.verification_method === 'dns-01'
                  ) &&
                  !(
                    (domain.status === 'pending' ||
                      domain.status === 'pending_http') &&
                    domain.verification_method === 'http-01'
                  ) &&
                  domain.status !== 'failed' && (
                    <>
                      <div className="flex items-center justify-between">
                        <h2 className="text-lg font-semibold">
                          SSL Certificate Provisioning
                        </h2>
                        <Badge variant={getStatusBadgeVariant(domain.status)}>
                          {domain.status}
                        </Badge>
                      </div>

                      {domain.verification_method === 'http-01' && (
                        <Alert>
                          <Globe className="h-4 w-4" />
                          <AlertTitle>HTTP-01 Challenge</AlertTitle>
                          <AlertDescription>
                            <p className="mb-2">
                              Your SSL certificate is being provisioned using
                              HTTP-01 validation.
                            </p>
                            <p className="text-sm text-muted-foreground">
                              Ensure your domain&apos;s A record points to your
                              server IP and port 80 is accessible.
                            </p>
                          </AlertDescription>
                        </Alert>
                      )}

                      {domain.verification_method === 'dns-01' && !order && (
                        <Alert>
                          <Info className="h-4 w-4" />
                          <AlertTitle>DNS-01 Challenge</AlertTitle>
                          <AlertDescription>
                            <p className="mb-4">
                              Create an ACME order to get started with DNS
                              validation.
                            </p>
                            <Button
                              onClick={handleCreateOrder}
                              disabled={
                                !canManageCertificates || createOrder.isPending
                              }
                            >
                              {createOrder.isPending ? (
                                <>
                                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                                  Creating Order...
                                </>
                              ) : (
                                <>
                                  <Shield className="mr-2 h-4 w-4" />
                                  Create ACME Order
                                </>
                              )}
                            </Button>
                          </AlertDescription>
                        </Alert>
                      )}

                      {domain.verification_method === 'dns-01' &&
                        order &&
                        !hasDnsValues && (
                          <Alert>
                            <Clock className="h-4 w-4" />
                            <AlertTitle>Waiting for Challenge Data</AlertTitle>
                            <AlertDescription>
                              The DNS challenge is being prepared. This usually
                              takes a few moments.
                            </AlertDescription>
                          </Alert>
                        )}
                    </>
                  )}
              </div>
            </Card>
          </div>

          {/* Sidebar */}
          <div className="space-y-6">
            {/* Domain Information */}
            <Card>
              <div className="p-6 space-y-4">
                <h2 className="text-lg font-semibold">Domain Information</h2>
                <div className="space-y-3">
                  <div className="space-y-1">
                    <span className="text-xs text-muted-foreground">
                      Status
                    </span>
                    <div>
                      <Badge variant={getStatusBadgeVariant(domain.status)}>
                        {domain.status}
                      </Badge>
                    </div>
                  </div>
                  <div className="space-y-1">
                    <span className="text-xs text-muted-foreground">
                      Verification Method
                    </span>
                    <p className="text-sm font-medium">
                      {domain.verification_method}
                    </p>
                  </div>
                  {domain.status === 'active' && (
                    <>
                      <Separator />
                      {!!domain.last_renewed && (
                        <div className="space-y-1">
                          <span className="text-xs text-muted-foreground">
                            Last Renewed
                          </span>
                          <p className="text-sm font-medium flex items-center gap-2">
                            <Clock className="h-4 w-4" />
                            {formatUTCDate(domain.last_renewed)}
                          </p>
                        </div>
                      )}
                      {!!domain.expiration_time && (
                        <div className="space-y-1">
                          <span className="text-xs text-muted-foreground">
                            Expires
                          </span>
                          <p className="text-sm font-medium flex items-center gap-2">
                            <Calendar className="h-4 w-4" />
                            {formatUTCDate(domain.expiration_time)}
                          </p>
                        </div>
                      )}
                    </>
                  )}
                  <Separator />
                  <div className="space-y-1">
                    <span className="text-xs text-muted-foreground">
                      Created
                    </span>
                    <p className="text-sm">
                      {formatUTCDate(domain.created_at)}
                    </p>
                  </div>
                  <div className="space-y-1">
                    <span className="text-xs text-muted-foreground">
                      Last Updated
                    </span>
                    <p className="text-sm">
                      {formatUTCDate(domain.updated_at)}
                    </p>
                  </div>
                </div>
              </div>
            </Card>

            {/* ACME Order Information */}
            {order && (
              <Card>
                <div className="p-6 space-y-4">
                  <h2 className="text-lg font-semibold">ACME Order</h2>
                  <div className="space-y-3">
                    <div className="space-y-1">
                      <span className="text-xs text-muted-foreground">
                        Order Status
                      </span>
                      <div>
                        <Badge variant={getStatusBadgeVariant(order.status)}>
                          {order.status}
                        </Badge>
                      </div>
                    </div>
                    <div className="space-y-1">
                      <span className="text-xs text-muted-foreground">
                        Order ID
                      </span>
                      <p className="text-sm font-mono">#{order.id}</p>
                    </div>
                    <div className="space-y-1">
                      <span className="text-xs text-muted-foreground">
                        Email
                      </span>
                      <p className="text-sm">{order.email}</p>
                    </div>
                    {order.expires_at && (
                      <div className="space-y-1">
                        <span className="text-xs text-muted-foreground">
                          Order Expires
                        </span>
                        <p className="text-sm">
                          {formatUTCDate(order.expires_at)}
                        </p>
                      </div>
                    )}
                    {hasDnsChallenge &&
                      domain.verification_method === 'dns-01' && (
                        <>
                          <Separator />
                          <div className="space-y-2">
                            <span className="text-xs text-muted-foreground">
                              DNS Challenge
                            </span>
                            <div className="space-y-3">
                              {dnsTxtRecords.map((record, index) => (
                                <div
                                  key={index}
                                  className="space-y-2 p-3 bg-muted/50 rounded-md"
                                >
                                  {dnsTxtRecords.length > 1 && (
                                    <div className="flex items-center justify-between">
                                      <span className="text-xs font-medium">
                                        Record {index + 1}
                                      </span>
                                      <Badge
                                        variant="outline"
                                        className="text-xs"
                                      >
                                        TXT
                                      </Badge>
                                    </div>
                                  )}
                                  <div className="space-y-1">
                                    <span className="text-xs font-medium">
                                      Name:
                                    </span>
                                    <div className="flex items-center gap-2">
                                      <code className="text-xs font-mono break-all flex-1">
                                        {record.name}
                                      </code>
                                      <Button
                                        size="sm"
                                        variant="ghost"
                                        className="h-6 w-6 p-0"
                                        onClick={() =>
                                          handleCopy(
                                            record.name,
                                            `sidebar-name-${index}`
                                          )
                                        }
                                      >
                                        {copiedField ===
                                        `sidebar-name-${index}` ? (
                                          <CopyCheck className="h-3 w-3" />
                                        ) : (
                                          <Copy className="h-3 w-3" />
                                        )}
                                      </Button>
                                    </div>
                                  </div>
                                  <div className="space-y-1">
                                    <span className="text-xs font-medium">
                                      Value:
                                    </span>
                                    <div className="flex items-center gap-2">
                                      <code className="text-xs font-mono break-all flex-1">
                                        {record.value}
                                      </code>
                                      <Button
                                        size="sm"
                                        variant="ghost"
                                        className="h-6 w-6 p-0"
                                        onClick={() =>
                                          handleCopy(
                                            record.value,
                                            `sidebar-value-${index}`
                                          )
                                        }
                                      >
                                        {copiedField ===
                                        `sidebar-value-${index}` ? (
                                          <CopyCheck className="h-3 w-3" />
                                        ) : (
                                          <Copy className="h-3 w-3" />
                                        )}
                                      </Button>
                                    </div>
                                  </div>
                                </div>
                              ))}
                            </div>
                          </div>
                        </>
                      )}
                    {order.error && (
                      <>
                        <Separator />
                        <div className="space-y-1">
                          <span className="text-xs text-muted-foreground text-destructive">
                            Error
                          </span>
                          <p className="text-sm text-destructive">
                            {order.error_type}
                          </p>
                          <p className="text-xs text-muted-foreground">
                            {order.error}
                          </p>
                        </div>
                      </>
                    )}
                    <Separator />
                    <div className="space-y-1">
                      <span className="text-xs text-muted-foreground">
                        Created
                      </span>
                      <p className="text-sm">
                        {formatUTCDate(order.created_at)}
                      </p>
                    </div>
                    <div className="space-y-1">
                      <span className="text-xs text-muted-foreground">
                        Updated
                      </span>
                      <p className="text-sm">
                        {formatUTCDate(order.updated_at)}
                      </p>
                    </div>
                  </div>
                </div>
              </Card>
            )}
          </div>
        </div>
      </div>
    </div>
  )
}
