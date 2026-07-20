import {
  cancelDomainOrderMutation,
  createOrRecreateOrderMutation,
  finalizeOrderMutation,
  getDomainByIdOptions,
  getDomainOrderOptions,
  getHttpChallengeDebugOptions,
  getPublicIpOptions,
  listDnsProvidersOptions as listProvidersOptions,
  renewDomainMutation,
  setupDnsChallengeMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from '@/components/ui/collapsible'
import { CopyButton } from '@/components/ui/copy-button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
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
import { formatExpiryRemaining, formatLocalDateTime, formatUTCDate } from '@/lib/date'
import {
  STATUS_ACTIVE_RENEWAL_FAILED,
  isServingCert,
} from '@/lib/domain-status'
import { useIsFetching, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { formatDistanceToNowStrict } from 'date-fns'
import {
  AlertTriangle,
  ArrowLeft,
  Calendar,
  CheckCircle,
  ChevronDown,
  Clock,
  ExternalLink,
  Globe,
  Info,
  Loader2,
  MoreHorizontal,
  RefreshCw,
  Shield,
  Wand2,
  XCircle,
} from 'lucide-react'
import { useCallback, useEffect, useState } from 'react'
import { useNavigate, useParams } from 'react-router-dom'
import { toast } from 'sonner'

type ChallengeData = {
  challenge_type: 'dns-01' | 'http-01'
  dns_txt_records: { name: string; value: string }[]
  key_authorization: string
  token: string
  validation_url: string
}

export function DomainDetail() {
  const { id } = useParams<{ id: string }>()
  const { setBreadcrumbs } = useBreadcrumbs()
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const [selectedDnsProvider, setSelectedDnsProvider] = useState<string>('')

  const domainQueryKey = getDomainByIdOptions({
    path: { domain: Number(id) },
  }).queryKey
  const orderQueryKey = getDomainOrderOptions({
    path: { domain_id: Number(id) },
  }).queryKey

  const { data: domain, isLoading: isDomainLoading } = useQuery({
    ...getDomainByIdOptions({ path: { domain: Number(id) } }),
    enabled: !!id,
  })

  const { data: order, isLoading: isOrderLoading } = useQuery({
    ...getDomainOrderOptions({ path: { domain_id: Number(id) } }),
    // Only fetch an in-progress ACME order when the domain isn't already serving a
    // cert. "active_renewal_failed" is still serving, so treat it like "active".
    enabled: !!id && !isServingCert(domain?.status),
    retry: false,
  })

  const { data: httpDebugInfo } = useQuery({
    ...getHttpChallengeDebugOptions({
      path: { domain: domain?.domain || '' },
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

  const fetchingCount = useIsFetching({
    predicate: (q) => {
      const k = q.queryKey?.[0]
      if (typeof k !== 'string') return false
      return (
        k.includes('getDomainById') ||
        k.includes('getDomainOrder') ||
        k.includes('getHttpChallengeDebug') ||
        k.includes('getPublicIp') ||
        k.includes('listProviders')
      )
    },
  })
  const isRefreshing = fetchingCount > 0

  const refreshAll = useCallback(
    async (opts?: { clearOrder?: boolean }) => {
      if (opts?.clearOrder) {
        queryClient.removeQueries({ queryKey: orderQueryKey })
      }
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: domainQueryKey }),
        queryClient.invalidateQueries({ queryKey: orderQueryKey }),
        queryClient.invalidateQueries({
          predicate: (q) => {
            const k = q.queryKey?.[0]
            return (
              typeof k === 'string' &&
              (k.includes('getHttpChallengeDebug') ||
                k.includes('getPublicIp') ||
                k.includes('listProviders'))
            )
          },
        }),
      ])
    },
    [queryClient, domainQueryKey, orderQueryKey]
  )

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
    meta: { errorTitle: 'Failed to create ACME order' },
    onSuccess: async () => {
      toast.success('ACME order created successfully')
      await refreshAll()
    },
  })

  const finalizeOrder = useMutation({
    ...finalizeOrderMutation(),
    meta: { errorTitle: 'Failed to verify DNS challenge' },
    onSuccess: async () => {
      toast.success(
        'DNS challenge verified! TLS certificate provisioning in progress.'
      )
      await refreshAll()

      const pollInterval = setInterval(async () => {
        await refreshAll()
        const latest = queryClient.getQueryData(domainQueryKey) as
          | { status?: string }
          | undefined
        if (latest?.status === 'active') {
          clearInterval(pollInterval)
          toast.success('TLS certificate is now active!')
          await refreshAll()
        }
      }, 3000)

      setTimeout(() => clearInterval(pollInterval), 120000)
    },
  })

  const cancelOrder = useMutation({
    ...cancelDomainOrderMutation(),
    meta: { errorTitle: 'Failed to cancel ACME order' },
    onSuccess: async () => {
      toast.success('ACME order cancelled. You can now start over.')
      await refreshAll({ clearOrder: true })
    },
  })

  const renewDomain = useMutation({
    ...renewDomainMutation(),
    meta: { errorTitle: 'Failed to renew certificate' },
    onSuccess: async (data) => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const response = data as any
      if (response?.txt_records && response.txt_records.length > 0) {
        toast.success(
          'Renewal order created. Update DNS TXT records and click "Verify & Complete" to finalize.',
          { duration: 6000 }
        )
      } else if (response?.Complete) {
        toast.success('Certificate renewed successfully')
      } else {
        toast.success('Certificate renewal initiated')
      }
      await refreshAll()
    },
  })

  const setupDns = useMutation({
    ...setupDnsChallengeMutation(),
    meta: { errorTitle: 'Failed to setup DNS records' },
    onSuccess: async (data) => {
      if (data.success) {
        toast.success(data.message)
      } else {
        toast.warning(data.message)
      }
      data.results.forEach((result) => {
        if (!result.success) {
          toast.error(`${result.name}: ${result.message}`)
        }
      })
      await refreshAll()
    },
  })

  const handleCreateOrder = () => {
    if (!domain) return
    createOrder.mutate({ path: { domain_id: domain.id } })
  }

  const handleCompleteDns = () => {
    if (!domain) return
    finalizeOrder.mutate({ path: { domain_id: domain.id } })
  }

  const handleSetupDnsRecords = async () => {
    if (!domain || !selectedDnsProvider) return
    await setupDns.mutateAsync({
      path: { domain_id: domain.id },
      body: { dns_provider_id: parseInt(selectedDnsProvider, 10) },
    })
  }

  const handleCancelOrder = () => {
    if (!domain) return
    toast.promise(
      cancelOrder.mutateAsync({ path: { domain_id: domain.id } }),
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
      await renewDomain.mutateAsync({ path: { domain: domain.domain } })
    } catch {
      // handled by meta.errorTitle
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
        return 'default' as const
      case STATUS_ACTIVE_RENEWAL_FAILED:
        return 'warning' as const
      case 'pending':
      case 'processing':
        return 'secondary' as const
      case 'failed':
      case 'invalid':
        return 'destructive' as const
      default:
        return 'outline' as const
    }
  }

  const isLoading = isDomainLoading || isOrderLoading

  if (isLoading && !domain) {
    return (
      <div className="flex-1 overflow-auto">
        <div className="flex items-center justify-center min-h-[400px]">
          <Loader2 className="size-8 animate-spin text-muted-foreground" />
        </div>
      </div>
    )
  }

  if (!domain) {
    return (
      <div className="flex-1 overflow-auto">
        <div className="max-w-5xl mx-auto p-6">
          <Alert variant="warning">
            <AlertTriangle className="size-4" />
            <AlertTitle>Domain not found</AlertTitle>
            <AlertDescription>
              The requested domain could not be found.
            </AlertDescription>
          </Alert>
          <Button className="mt-4" onClick={() => navigate('/domains')}>
            <ArrowLeft className="mr-2 size-4" />
            Back to Domains
          </Button>
        </div>
      </div>
    )
  }

  // Terminal ACME order states (per RFC 8555): cancelled, invalid, revoked, deactivated.
  // When the order is in any of these, no further action is possible on it — the user
  // must create a new order. Treat the order as "absent" for UI gating purposes so we
  // don't strand the user with hidden actions.
  const TERMINAL_ORDER_STATUSES = ['cancelled', 'canceled', 'invalid', 'revoked', 'deactivated']
  const isOrderTerminal = !!order && TERMINAL_ORDER_STATUSES.includes(order.status)
  const activeOrder = order && !isOrderTerminal ? order : undefined

  const challengeData = activeOrder?.authorizations as ChallengeData | undefined

  // The domain's verification_method is normally 'dns-01' or 'http-01', but legacy rows
  // (and tls-alpn-01) carry values the UI has no branch for — which previously rendered
  // an empty card (blank page) while the domain sat in challenge_requested. The ACME
  // order itself always stores the concrete challenge type it was created with
  // (authorizations.challenge_type), so prefer that when the domain field is unrecognised.
  const KNOWN_METHODS = ['dns-01', 'http-01'] as const
  type KnownMethod = (typeof KNOWN_METHODS)[number]
  const isKnownMethod = (m?: string | null): m is KnownMethod =>
    !!m && (KNOWN_METHODS as readonly string[]).includes(m)
  const effectiveMethod: KnownMethod | undefined = isKnownMethod(domain.verification_method)
    ? domain.verification_method
    : isKnownMethod(challengeData?.challenge_type)
      ? challengeData?.challenge_type
      : undefined

  const hasDnsChallenge = !!activeOrder && effectiveMethod === 'dns-01'
  const dnsTxtRecords = challengeData?.dns_txt_records || []
  const hasDnsValues = dnsTxtRecords.length > 0
  const hasHttpChallenge =
    !!activeOrder && effectiveMethod === 'http-01' && !!challengeData

  const isPendingState =
    domain.status === 'challenge_requested' ||
    domain.status === 'pending_dns' ||
    domain.status === 'pending_http' ||
    domain.status === 'pending'

  // The user can (re)create an order when the domain is awaiting issuance and either
  // no order exists or the existing order is in a terminal state.
  const canCreateOrder = isPendingState && !activeOrder


  // Renew is meaningful for any ACME-issued certificate. DNS-01 renewals
  // require the user to re-add TXT records, so we expose it as "Start renewal"
  // instead of "Renew". Legacy/unknown values (e.g. "acme") are treated as
  // auto-renewable since the backend resolves the challenge type.
  const canRenew = canManageCertificates && !!domain.verification_method
  const renewLabel =
    domain.verification_method === 'dns-01' ? 'Start renewal' : 'Renew certificate'

  const primaryActionButton = (() => {
    if (isServingCert(domain.status)) return null
    if (canCreateOrder) {
      return (
        <Button
          onClick={handleCreateOrder}
          disabled={!canManageCertificates || createOrder.isPending}
        >
          {createOrder.isPending ? (
            <>
              <Loader2 className="mr-2 size-4 animate-spin" />
              Creating order…
            </>
          ) : (
            <>
              <Shield className="mr-2 size-4" />
              {isOrderTerminal ? 'Create new order' : 'Create order'}
            </>
          )}
        </Button>
      )
    }
    if (activeOrder && (hasDnsChallenge || hasHttpChallenge)) {
      return (
        <Button
          onClick={handleCompleteDns}
          disabled={!canManageCertificates || finalizeOrder.isPending}
        >
          {finalizeOrder.isPending ? (
            <>
              <Loader2 className="mr-2 size-4 animate-spin" />
              Verifying…
            </>
          ) : (
            <>
              <CheckCircle className="mr-2 size-4" />
              Verify & finalize
            </>
          )}
        </Button>
      )
    }
    return null
  })()

  // Shared compact TXT record row
  const DnsRecordsList = ({ keyPrefix }: { keyPrefix: string }) =>
    hasDnsValues ? (
      <div className="divide-y divide-gray-950/5 rounded-lg border border-gray-950/10 overflow-hidden">
        {dnsTxtRecords.map((record, index) => (
          <div key={`${keyPrefix}-${index}`} className="grid grid-cols-[auto_1fr_auto] items-start gap-x-3 gap-y-1 px-3 py-2.5 sm:px-4">
            <Badge variant="outline" className="mt-0.5">TXT</Badge>
            <div className="min-w-0 space-y-1">
              <p className="font-mono text-xs break-all text-foreground">
                <span className="text-muted-foreground">Name:</span> {record.name}
              </p>
              <p className="font-mono text-xs break-all text-foreground">
                <span className="text-muted-foreground">Value:</span> {record.value}
              </p>
            </div>
            <div className="flex flex-col gap-1">
              <CopyButton
                value={record.name}
                minimal
                className="h-7 rounded-md border border-gray-950/10 px-2 py-1 text-xs"
              >
                <span className="hidden sm:inline">Name</span>
              </CopyButton>
              <CopyButton
                value={record.value}
                minimal
                className="h-7 rounded-md border border-gray-950/10 px-2 py-1 text-xs"
              >
                <span className="hidden sm:inline">Value</span>
              </CopyButton>
            </div>
          </div>
        ))}
      </div>
    ) : null

  const DnsAutoProvision = () =>
    dnsProviders && dnsProviders.length > 0 ? (
      <div className="flex flex-col gap-3 rounded-lg border border-gray-950/10 bg-muted/40 p-4 sm:flex-row sm:items-center">
        <div className="flex items-start gap-3 flex-1">
          <div className="rounded-md bg-primary/10 p-2 shrink-0">
            <Wand2 className="size-4 text-primary" />
          </div>
          <div className="min-w-0">
            <p className="text-sm font-medium">Auto-provision records</p>
            <p className="text-xs text-muted-foreground">
              Create TXT records using a configured DNS provider.
            </p>
          </div>
        </div>
        <div className="flex flex-col gap-2 sm:flex-row sm:shrink-0">
          <Select value={selectedDnsProvider} onValueChange={setSelectedDnsProvider}>
            <SelectTrigger className="w-full sm:w-[200px]">
              <SelectValue placeholder="Select provider" />
            </SelectTrigger>
            <SelectContent>
              {dnsProviders.map((provider) => (
                <SelectItem key={provider.id} value={provider.id.toString()}>
                  {provider.name} ({provider.provider_type})
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          <Button
            size="sm"
            variant="outline"
            onClick={handleSetupDnsRecords}
            disabled={
              !selectedDnsProvider || setupDns.isPending || !canManageCertificates
            }
          >
            {setupDns.isPending ? (
              <>
                <Loader2 className="mr-2 size-4 animate-spin" />
                Creating…
              </>
            ) : (
              <>
                <Wand2 className="mr-2 size-4" />
                Auto-create
              </>
            )}
          </Button>
        </div>
      </div>
    ) : null

  const CurrentVariant = (
    <div className="grid grid-cols-1 lg:grid-cols-3 gap-6">
      <div className="lg:col-span-2 space-y-6">
        <Card>
          <div className="p-6 space-y-4">
            {domain.status === STATUS_ACTIVE_RENEWAL_FAILED && (
              <Alert variant="warning">
                <AlertTriangle className="size-4" />
                <AlertTitle>Certificate renewal failed</AlertTitle>
                <AlertDescription>
                  The current certificate is still valid and being served, but the
                  last automatic renewal attempt failed. Renew it before it expires
                  to avoid downtime.
                  {domain.last_error ? ` Last error: ${domain.last_error}` : ''}
                </AlertDescription>
              </Alert>
            )}

            {isServingCert(domain.status) && (
              <ActiveCertificateInner
                domain={domain}
                onRenew={canRenew ? handleRenewDomain : undefined}
                renewLabel={renewLabel}
                renewing={renewDomain.isPending}
                withHeader
              />
            )}

            {isPendingState && effectiveMethod === 'dns-01' && (
              <>
                <div className="flex items-center justify-between">
                  <h2 className="text-lg font-semibold">DNS challenge required</h2>
                  <Badge variant={getStatusBadgeVariant(domain.status)}>{domain.status}</Badge>
                </div>
                {!activeOrder ? (
                  <div className="space-y-3 rounded-lg border border-gray-950/10 p-4">
                    <p className="text-sm text-muted-foreground">
                      {isOrderTerminal
                        ? `Previous order ended with status "${order?.status}". Create a new ACME order to continue.`
                        : 'Create an ACME order to get your DNS challenge token.'}
                    </p>
                    <Button
                      onClick={handleCreateOrder}
                      disabled={!canManageCertificates || createOrder.isPending}
                    >
                      {createOrder.isPending ? (
                        <>
                          <Loader2 className="mr-2 size-4 animate-spin" />
                          Creating…
                        </>
                      ) : (
                        <>
                          <Shield className="mr-2 size-4" />
                          {isOrderTerminal ? 'Create new order' : 'Create order'}
                        </>
                      )}
                    </Button>
                  </div>
                ) : hasDnsValues ? (
                  <div className="overflow-hidden rounded-lg border border-gray-950/10">
                    <InlineStepStrip
                      steps={[
                        { label: `Add record${dnsTxtRecords.length > 1 ? 's' : ''}`, state: 'current' },
                        { label: 'Wait for propagation', state: 'upcoming' },
                        { label: 'Verify & finalize', state: 'upcoming' },
                      ]}
                    />
                    <div className="space-y-4 border-t border-gray-950/5 p-4">
                      <div>
                        <p className="text-sm font-medium">
                          Add TXT record{dnsTxtRecords.length > 1 ? 's' : ''} to your DNS provider
                        </p>
                        <p className="text-xs text-muted-foreground">
                          Propagation typically takes 5–15 min (up to 24 h).
                          {dnsTxtRecords[0] && (
                            <>
                              {' '}
                              <a
                                href={`https://www.whatsmydns.net/#TXT/${dnsTxtRecords[0].name}`}
                                target="_blank"
                                rel="noopener noreferrer"
                                className="inline-flex items-center gap-1 underline text-foreground"
                              >
                                Check propagation
                                <ExternalLink className="size-3" />
                              </a>
                            </>
                          )}
                        </p>
                      </div>
                      <DnsRecordsList keyPrefix="current-main" />
                      <DnsAutoProvision />
                      <div className="flex flex-wrap gap-2 pt-1">
                        <Button
                          onClick={handleCompleteDns}
                          disabled={finalizeOrder.isPending || !canManageCertificates}
                        >
                          {finalizeOrder.isPending ? (
                            <>
                              <Loader2 className="mr-2 size-4 animate-spin" />
                              Verifying…
                            </>
                          ) : (
                            <>
                              <CheckCircle className="mr-2 size-4" />
                              Verify &amp; finalize
                            </>
                          )}
                        </Button>
                        <Button
                          variant="outline"
                          onClick={handleCancelOrder}
                          disabled={!canManageCertificates}
                        >
                          <XCircle className="mr-2 size-4" />
                          Cancel order
                        </Button>
                      </div>
                    </div>
                  </div>
                ) : (
                  <Alert>
                    <Clock className="size-4" />
                    <AlertTitle>Waiting for challenge data</AlertTitle>
                    <AlertDescription>
                      The DNS challenge is being prepared. This usually takes a few moments.
                    </AlertDescription>
                  </Alert>
                )}
              </>
            )}

            {isPendingState && effectiveMethod === 'http-01' && (
              <>
                {hasHttpChallenge && challengeData ? (
                  <HttpChallengePanel
                    domain={domain}
                    challengeData={challengeData}
                    publicIpData={publicIpData}
                    httpDebugInfo={httpDebugInfo}
                    onVerify={handleCompleteDns}
                    onCancel={handleCancelOrder}
                    verifying={finalizeOrder.isPending}
                    canManage={canManageCertificates}
                    withHeader
                  />
                ) : !activeOrder ? (
                  <>
                    <div className="flex items-center justify-between">
                      <h2 className="text-lg font-semibold">HTTP-01 challenge required</h2>
                      <Badge variant={getStatusBadgeVariant(domain.status)}>{domain.status}</Badge>
                    </div>
                    <div className="space-y-3 rounded-lg border border-gray-950/10 p-4">
                      <p className="text-sm text-muted-foreground">
                        {isOrderTerminal
                          ? `Previous order ended with status "${order?.status}". Create a new ACME order to continue.`
                          : 'Create an ACME order to get your HTTP-01 challenge token.'}
                      </p>
                      <Button
                        onClick={handleCreateOrder}
                        disabled={!canManageCertificates || createOrder.isPending}
                      >
                        {createOrder.isPending ? (
                          <>
                            <Loader2 className="mr-2 size-4 animate-spin" />
                            Creating…
                          </>
                        ) : (
                          <>
                            <Shield className="mr-2 size-4" />
                            {isOrderTerminal ? 'Create new order' : 'Create order'}
                          </>
                        )}
                      </Button>
                    </div>
                  </>
                ) : (
                  <Alert>
                    <Clock className="size-4" />
                    <AlertTitle>Waiting for challenge data</AlertTitle>
                    <AlertDescription>
                      The HTTP challenge is being prepared. This usually takes a few moments.
                    </AlertDescription>
                  </Alert>
                )}
              </>
            )}

            {/* Fallback: pending domain whose verification method we can't classify
                (legacy "acme", "tls-alpn-01", or null — with no order to disambiguate).
                Previously this rendered an empty card (blank page). Surface the order
                status and the create/verify path so the user is never stranded. */}
            {isPendingState && !effectiveMethod && (
              <>
                <div className="flex items-center justify-between">
                  <h2 className="text-lg font-semibold">Certificate challenge required</h2>
                  <Badge variant={getStatusBadgeVariant(domain.status)}>{domain.status}</Badge>
                </div>
                <div className="space-y-3 rounded-lg border border-gray-950/10 p-4">
                  <p className="text-sm text-muted-foreground">
                    {!activeOrder
                      ? isOrderTerminal
                        ? `Previous order ended with status "${order?.status}". Create a new ACME order to continue.`
                        : 'Create an ACME order to begin certificate provisioning for this domain.'
                      : 'An ACME order exists for this domain. Once any required DNS or HTTP records are in place, verify to finalize the certificate.'}
                  </p>
                  <div className="flex flex-wrap gap-2">
                    {canCreateOrder ? (
                      <Button
                        onClick={handleCreateOrder}
                        disabled={!canManageCertificates || createOrder.isPending}
                      >
                        {createOrder.isPending ? (
                          <>
                            <Loader2 className="mr-2 size-4 animate-spin" />
                            Creating…
                          </>
                        ) : (
                          <>
                            <Shield className="mr-2 size-4" />
                            {isOrderTerminal ? 'Create new order' : 'Create order'}
                          </>
                        )}
                      </Button>
                    ) : (
                      <>
                        <Button
                          onClick={handleCompleteDns}
                          disabled={finalizeOrder.isPending || !canManageCertificates}
                        >
                          {finalizeOrder.isPending ? (
                            <>
                              <Loader2 className="mr-2 size-4 animate-spin" />
                              Verifying…
                            </>
                          ) : (
                            <>
                              <CheckCircle className="mr-2 size-4" />
                              Verify &amp; finalize
                            </>
                          )}
                        </Button>
                        <Button
                          variant="outline"
                          onClick={handleCancelOrder}
                          disabled={!canManageCertificates}
                        >
                          <XCircle className="mr-2 size-4" />
                          Cancel order
                        </Button>
                      </>
                    )}
                  </div>
                  {hasDnsValues && (
                    <div className="space-y-2 pt-1">
                      <p className="text-xs text-muted-foreground">
                        Challenge record{dnsTxtRecords.length > 1 ? 's' : ''} for this order:
                      </p>
                      <DnsRecordsList keyPrefix="current-fallback" />
                    </div>
                  )}
                </div>
              </>
            )}

            {domain.status === 'failed' && (
              <FailedPanel
                domain={domain}
                dnsTxtRecords={dnsTxtRecords}
                onRetry={handleCompleteDns}
                onCancel={handleCancelOrder}
                retrying={finalizeOrder.isPending}
                canManage={canManageCertificates}
                keyPrefix="current-failed"
                withHeader
              />
            )}
          </div>
        </Card>
      </div>

      <div className="space-y-6">
        {order?.error && (
          <Card className="border-destructive/30">
            <div className="p-5 space-y-1.5">
              <div className="flex items-center gap-2">
                <AlertTriangle className="size-4 text-destructive" />
                <h3 className="text-sm font-semibold text-destructive">Order error</h3>
              </div>
              <p className="text-sm font-medium text-destructive">{order.error_type}</p>
              <p className="text-xs text-muted-foreground">{order.error}</p>
            </div>
          </Card>
        )}

        <Card>
          <div className="p-5 space-y-4">
            <h2 className="text-sm font-semibold">Key facts</h2>
            <dl className="grid grid-cols-2 gap-x-4 gap-y-3">
              <KeyFact label="Method" value={domain.verification_method} />
              <KeyFact
                label="Wildcard"
                value={domain.is_wildcard ? 'Yes' : 'No'}
              />
              {domain.certificate && (
                <KeyFact
                  label="Certificate"
                  value={
                    <span className="text-emerald-600 dark:text-emerald-400 font-medium">
                      Present
                    </span>
                  }
                />
              )}
              {domain.expiration_time && (
                <KeyFact
                  label="Expires"
                  value={
                    <span className={isExpiringSoon(domain.expiration_time) ? 'text-amber-600 dark:text-amber-400 font-medium' : ''}>
                      {formatLocalDateTime(domain.expiration_time)}
                    </span>
                  }
                />
              )}
              {domain.last_renewed && (
                <KeyFact
                  label="Last issued"
                  value={formatLocalDateTime(domain.last_renewed)}
                />
              )}
            </dl>
          </div>
        </Card>

        {order && (
          <Card>
            <div className="p-5 space-y-4">
              <div className="flex items-center justify-between">
                <h2 className="text-sm font-semibold">ACME order</h2>
                <Badge variant={getStatusBadgeVariant(order.status)}>
                  {order.status}
                </Badge>
              </div>
              <dl className="space-y-2">
                <CompactDl label="Order ID" value={`#${order.id}`} mono />
                <CompactDl label="Email" value={order.email} />
                {order.expires_at && (
                  <CompactDl label="Expires" value={formatLocalDateTime(order.expires_at)} />
                )}
                <CompactDl label="Created" value={formatLocalDateTime(order.created_at)} />
                <CompactDl label="Updated" value={formatLocalDateTime(order.updated_at)} />
              </dl>
            </div>
          </Card>
        )}
      </div>
    </div>
  )

  return (
    <div className="flex-1 overflow-auto">
      <div className="w-full space-y-6 p-4 sm:p-6">
        {/* Header (shared across variants) */}
        <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between sm:gap-4">
          <div className="flex items-start gap-3 min-w-0 sm:items-center sm:gap-4">
            <Button
              variant="ghost"
              size="icon"
              className="shrink-0"
              onClick={() => navigate('/domains')}
            >
              <ArrowLeft className="size-4" />
            </Button>
            <div className="min-w-0">
              <div className="flex flex-wrap items-center gap-2 sm:gap-3">
                <h1 className="text-xl sm:text-2xl font-semibold tracking-tight truncate">
                  {domain.domain}
                </h1>
                <Badge variant={getStatusBadgeVariant(domain.status)}>{domain.status}</Badge>
                {domain.is_wildcard && <Badge variant="outline">Wildcard</Badge>}
              </div>
              <p className="text-sm text-muted-foreground mt-0.5">
                TLS certificate &amp; order management
              </p>
            </div>
          </div>
          <div className="flex flex-wrap items-center gap-2">
            {primaryActionButton}
            <Button
              variant="outline"
              size="sm"
              onClick={() => refreshAll()}
              disabled={isRefreshing}
            >
              {isRefreshing ? (
                <Loader2 className="mr-2 size-4 animate-spin" />
              ) : (
                <RefreshCw className="mr-2 size-4" />
              )}
              Refresh
            </Button>
            {(canRenew || canCreateOrder || (activeOrder && isPendingState)) && (
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <Button variant="outline" size="icon" aria-label="More actions">
                    <MoreHorizontal className="size-4" />
                  </Button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="end" className="w-48">
                  {canRenew && isServingCert(domain.status) && (
                    <DropdownMenuItem
                      onSelect={() => handleRenewDomain()}
                      disabled={renewDomain.isPending}
                    >
                      <RefreshCw className="mr-2 size-4" />
                      {renewLabel}
                    </DropdownMenuItem>
                  )}
                  {canCreateOrder && (
                    <DropdownMenuItem
                      onSelect={handleCreateOrder}
                      disabled={!canManageCertificates || createOrder.isPending}
                    >
                      <Shield className="mr-2 size-4" />
                      {isOrderTerminal ? 'Create new order' : 'Create order'}
                    </DropdownMenuItem>
                  )}
                  {activeOrder && isPendingState && (
                    <>
                      {canRenew && isServingCert(domain.status) && <DropdownMenuSeparator />}
                      <DropdownMenuItem
                        onSelect={handleCancelOrder}
                        disabled={!canManageCertificates || cancelOrder.isPending}
                        className="text-destructive focus:text-destructive"
                      >
                        <XCircle className="mr-2 size-4" />
                        Cancel order
                      </DropdownMenuItem>
                    </>
                  )}
                </DropdownMenuContent>
              </DropdownMenu>
            )}
          </div>
        </div>

        {/* Alerts (shared across variants) */}
        {isUsingCloudflare() && (
          <Alert className="border-purple-200 bg-purple-50/50 dark:bg-purple-950/10">
            <Info className="size-4 text-purple-600" />
            <AlertDescription>
              Domain and certificate management is handled automatically by Cloudflare Tunnel.
            </AlertDescription>
          </Alert>
        )}

        {isServingCert(domain.status) && isExpiringSoon(domain.expiration_time) && (
          <ExpiringSoonAlert
            expirationTime={domain.expiration_time}
            canRenew={canRenew}
            renewLabel={renewLabel}
            onRenew={handleRenewDomain}
            renewing={renewDomain.isPending}
          />
        )}

        {domain.last_error && domain.status !== 'failed' && (
          <Alert variant="warning">
            <AlertTriangle className="size-4" />
            <AlertTitle>Error: {domain.last_error_type || 'Certificate error'}</AlertTitle>
            <AlertDescription>{domain.last_error}</AlertDescription>
          </Alert>
        )}

        {CurrentVariant}

        {effectiveMethod === 'http-01' &&
          isPendingState &&
          !hasHttpChallenge && (
            <Alert>
              <Globe className="size-4" />
              <AlertTitle>HTTP-01 challenge</AlertTitle>
              <AlertDescription>
                Your TLS certificate is being provisioned using HTTP-01 validation.
                Ensure your domain&apos;s A record points to your server IP and port 80 is accessible.
              </AlertDescription>
            </Alert>
          )}
      </div>
    </div>
  )
}

// ============================================================================
// Sub-components
// ============================================================================

type Domain = {
  id: number
  domain: string
  status: string
  verification_method: string
  is_wildcard?: boolean
  last_renewed?: number | null
  expiration_time?: number | null
  certificate?: string | null
  last_error?: string | null
  last_error_type?: string | null
  created_at: number
  updated_at: number
}


function ExpiringSoonAlert({
  expirationTime,
  canRenew,
  renewLabel,
  onRenew,
  renewing,
}: {
  expirationTime?: number | null
  canRenew: boolean
  renewLabel: string
  onRenew: () => void
  renewing: boolean
}) {
  const remaining = expirationTime ? formatExpiryRemaining(expirationTime) : null
  const variant = remaining?.expired || (remaining && remaining.totalHours < 48)
    ? 'destructive'
    : 'warning'
  return (
    <Alert variant={variant as 'destructive' | 'warning'}>
      <AlertTriangle className="size-4" />
      <AlertTitle>
        {remaining?.expired
          ? `Certificate expired ${remaining.short} ago`
          : `Certificate expiring soon${remaining ? ` — in ${remaining.short}` : ''}`}
      </AlertTitle>
      <AlertDescription className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <span>
          The TLS certificate {remaining?.expired ? 'expired' : 'will expire'} on{' '}
          {formatLocalDateTime(expirationTime || 0)}. Renew it before expiration to avoid service interruption.
        </span>
        {canRenew && (
          <Button size="sm" onClick={onRenew} disabled={renewing}>
            <RefreshCw className="mr-2 size-4" />
            {renewLabel}
          </Button>
        )}
      </AlertDescription>
    </Alert>
  )
}

function KeyFact({ label, value }: { label: string; value: React.ReactNode }) {
  return (
    <div className="min-w-0">
      <dt className="text-xs font-medium text-muted-foreground">{label}</dt>
      <dd className="mt-0.5 text-sm font-medium truncate tabular-nums">{value}</dd>
    </div>
  )
}

function CompactDl({
  label,
  value,
  mono,
}: {
  label: string
  value: React.ReactNode
  mono?: boolean
}) {
  return (
    <div className="flex items-baseline justify-between gap-3">
      <dt className="text-xs font-medium text-muted-foreground shrink-0">{label}</dt>
      <dd
        className={
          mono
            ? 'text-xs font-mono text-right truncate min-w-0'
            : 'text-sm text-right truncate min-w-0'
        }
      >
        {value}
      </dd>
    </div>
  )
}

type InlineStep = {
  label: string
  state: 'current' | 'upcoming' | 'complete'
}

function InlineStepStrip({ steps }: { steps: InlineStep[] }) {
  return (
    <ol className="flex items-stretch divide-x divide-gray-950/5 bg-muted/40">
      {steps.map((step, idx) => {
        const stateClasses =
          step.state === 'current'
            ? 'text-foreground'
            : step.state === 'complete'
            ? 'text-muted-foreground'
            : 'text-muted-foreground/70'
        const indicatorClasses =
          step.state === 'current'
            ? 'bg-foreground text-background'
            : step.state === 'complete'
            ? 'bg-foreground/70 text-background'
            : 'bg-muted-foreground/20 text-muted-foreground'
        return (
          <li
            key={idx}
            className={`flex flex-1 items-center gap-2 px-3 py-2 text-xs font-medium ${stateClasses}`}
          >
            <span
              className={`inline-flex size-5 items-center justify-center rounded-full text-[11px] tabular-nums ${indicatorClasses}`}
            >
              {idx + 1}
            </span>
            <span className="truncate">{step.label}</span>
          </li>
        )
      })}
    </ol>
  )
}



function ActiveCertificateInner({
  domain,
  onRenew,
  renewLabel = 'Renew certificate',
  canManage,
  renewing,
  withHeader,
}: {
  domain: Domain
  onRenew?: () => void
  renewLabel?: string
  canManage?: boolean
  renewing?: boolean
  withHeader?: boolean
}) {
  const [pemOpen, setPemOpen] = useState(false)
  return (
    <>
      {withHeader && (
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-2">
            <CheckCircle className="size-5 text-green-600" />
            <h2 className="text-lg font-semibold">Active TLS certificate</h2>
          </div>
          {onRenew && (
            <Button
              onClick={onRenew}
              variant="outline"
              size="sm"
              disabled={(canManage === false) || renewing}
            >
              <RefreshCw className="mr-2 size-4" />
              {renewLabel}
            </Button>
          )}
        </div>
      )}
      <Alert className="border-green-200 bg-green-50/50 dark:bg-green-950/10">
        <CheckCircle className="size-4 text-green-600" />
        <AlertDescription>
          Your TLS certificate is active and your domain is secured with HTTPS.
        </AlertDescription>
      </Alert>
      <div className="grid grid-cols-1 gap-4 rounded-lg bg-muted/50 p-4 md:grid-cols-2">
        {domain.last_renewed ? (
          <div className="space-y-1">
            <span className="text-xs font-medium text-muted-foreground">Last renewed</span>
            <p
              className="text-sm font-medium flex items-center gap-2"
              title={formatUTCDate(domain.last_renewed)}
            >
              <Clock className="size-4" />
              {formatDistanceToNowStrict(new Date(domain.last_renewed), { addSuffix: true })}
            </p>
          </div>
        ) : null}
        {domain.expiration_time ? (
          <div className="space-y-1">
            <span className="text-xs font-medium text-muted-foreground">Expires</span>
            <p
              className="text-sm font-medium flex items-center gap-2"
              title={formatUTCDate(domain.expiration_time)}
            >
              <Calendar className="size-4" />
              in {formatDistanceToNowStrict(new Date(domain.expiration_time))}
            </p>
          </div>
        ) : null}
      </div>
      {domain.certificate && (
        <Collapsible open={pemOpen} onOpenChange={setPemOpen}>
          <div className="flex items-center justify-between">
            <CollapsibleTrigger asChild>
              <Button variant="ghost" size="sm" className="-ml-2 px-2">
                <ChevronDown
                  className={
                    pemOpen
                      ? 'mr-2 size-4 rotate-180 transition-transform'
                      : 'mr-2 size-4 transition-transform'
                  }
                />
                {pemOpen ? 'Hide certificate' : 'Show certificate (PEM)'}
              </Button>
            </CollapsibleTrigger>
            <CopyButton
              value={domain.certificate}
              minimal
              className="size-8 rounded-md"
            />
          </div>
          <CollapsibleContent>
            <pre className="mt-2 p-4 bg-muted rounded-lg text-xs font-mono overflow-x-auto max-h-64 overflow-y-auto">
              {domain.certificate}
            </pre>
          </CollapsibleContent>
        </Collapsible>
      )}
    </>
  )
}

function HttpChallengePanel({
  domain,
  challengeData,
  publicIpData,
  httpDebugInfo,
  onVerify,
  onCancel,
  verifying,
  canManage,
  hideActions,
  withHeader,
}: {
  domain: Domain
  challengeData: ChallengeData
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  publicIpData: any
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  httpDebugInfo: any
  onVerify: () => void
  onCancel: () => void
  verifying: boolean
  canManage: boolean
  hideActions?: boolean
  withHeader?: boolean
}) {
  const ip =
    publicIpData && typeof publicIpData === 'object' && 'ip' in publicIpData
      ? (publicIpData.ip as string)
      : undefined
  const dnsName = domain.is_wildcard
    ? `*.${domain.domain.replace('*.', '')}`
    : domain.domain
  const challengeUrl = `http://${domain.domain}/.well-known/acme-challenge/${challengeData.token}`

  return (
    <div className="space-y-4">
      {withHeader && (
        <div className="flex items-center justify-between">
          <h2 className="text-lg font-semibold">HTTP-01 challenge</h2>
        </div>
      )}

      {ip && (
        <div className="rounded-lg border border-gray-950/10 bg-muted/40">
          <div className="flex items-center justify-between border-b border-gray-950/5 px-4 py-2.5">
            <div className="flex items-center gap-2">
              <Info className="size-4 text-blue-600" />
              <span className="text-sm font-medium">DNS A record required</span>
            </div>
            <a
              href={`https://www.whatsmydns.net/#A/${domain.domain.replace('*.', '')}`}
              target="_blank"
              rel="noopener noreferrer"
              className="inline-flex items-center gap-1 text-xs text-blue-600 hover:underline"
            >
              Check propagation
              <ExternalLink className="size-3" />
            </a>
          </div>
          <dl className="divide-y divide-gray-950/5 text-sm">
            <DnsKvRow label="Name" value={dnsName} />
            <DnsKvRow label="Type" value="A" copyable={false} />
            <DnsKvRow label="Value" value={ip} />
          </dl>
        </div>
      )}

      <div className="space-y-3 rounded-lg border border-gray-950/10 p-4">
        <div className="flex items-center justify-between">
          <span className="text-sm font-medium">Challenge URL</span>
          <a
            href="https://letsdebug.net"
            target="_blank"
            rel="noopener noreferrer"
            className="inline-flex items-center gap-1 text-xs text-blue-600 hover:underline"
          >
            Let&apos;s Debug
            <ExternalLink className="size-3" />
          </a>
        </div>
        <div className="flex items-center gap-2">
          <code className="flex-1 p-2.5 bg-muted rounded text-xs font-mono break-all">
            {challengeUrl}
          </code>
          <CopyButton value={challengeUrl} minimal className="size-8 rounded-md" />
        </div>
        <div>
          <span className="text-sm font-medium">Expected response</span>
        </div>
        <div className="flex items-center gap-2">
          <code className="flex-1 p-2.5 bg-muted rounded text-xs font-mono break-all">
            {challengeData.key_authorization}
          </code>
          <CopyButton
            value={challengeData.key_authorization}
            minimal
            className="size-8 rounded-md"
          />
        </div>
      </div>

      {httpDebugInfo?.dns_error && (
        <Alert variant="destructive">
          <AlertTriangle className="size-4" />
          <AlertTitle>DNS configuration issue</AlertTitle>
          <AlertDescription>
            <p>{httpDebugInfo.dns_error}</p>
            <p className="text-sm mt-2">
              Ensure your domain&apos;s A record points to your server IP address.
            </p>
          </AlertDescription>
        </Alert>
      )}

      {!httpDebugInfo?.dns_error &&
        httpDebugInfo &&
        httpDebugInfo.dns_a_records.length === 0 && (
          <Alert variant="warning">
            <AlertTriangle className="size-4" />
            <AlertTitle>No DNS records found</AlertTitle>
            <AlertDescription>
              Your domain doesn&apos;t have any A records pointing to a server.
            </AlertDescription>
          </Alert>
        )}

      {!httpDebugInfo?.dns_error &&
        httpDebugInfo &&
        httpDebugInfo.dns_a_records.length > 0 &&
        httpDebugInfo.challenge_exists && (
          <Alert className="border-green-200 bg-green-50/50 dark:bg-green-950/10">
            <CheckCircle className="size-4 text-green-600" />
            <AlertTitle>Ready for validation</AlertTitle>
            <AlertDescription>
              Your domain is pointing to the server and the challenge is ready.
            </AlertDescription>
          </Alert>
        )}

      {!hideActions && (
        <div className="flex flex-wrap gap-2">
          <Button onClick={onVerify} disabled={verifying || !canManage}>
            {verifying ? (
              <>
                <Loader2 className="mr-2 size-4 animate-spin" />
                Verifying…
              </>
            ) : (
              <>
                <CheckCircle className="mr-2 size-4" />
                Verify & provision TLS
              </>
            )}
          </Button>
          <Button variant="outline" onClick={onCancel} disabled={!canManage}>
            <XCircle className="mr-2 size-4" />
            Cancel order
          </Button>
        </div>
      )}
    </div>
  )
}

function DnsKvRow({
  label,
  value,
  copyable = true,
}: {
  label: string
  value: string
  copyable?: boolean
}) {
  return (
    <div className="grid grid-cols-[auto_1fr_auto] items-center gap-3 px-4 py-2.5">
      <dt className="text-xs font-medium text-muted-foreground w-16">{label}</dt>
      <dd className="min-w-0">
        <code className="text-xs font-mono break-all">{value}</code>
      </dd>
      {copyable ? (
        <CopyButton value={value} minimal className="size-8 rounded-md" />
      ) : (
        <span />
      )}
    </div>
  )
}

function FailedPanel({
  domain,
  dnsTxtRecords,
  onRetry,
  onCancel,
  retrying,
  canManage,
  keyPrefix,
  withHeader,
}: {
  domain: Domain
  dnsTxtRecords: { name: string; value: string }[]
  onRetry: () => void
  onCancel: () => void
  retrying: boolean
  canManage: boolean
  keyPrefix: string
  withHeader?: boolean
}) {
  return (
    <div className="space-y-4">
      {withHeader && (
        <div className="flex items-center justify-between">
          <h2 className="text-lg font-semibold">Verification failed</h2>
          <Badge variant="destructive">{domain.status}</Badge>
        </div>
      )}
      <Alert variant="destructive">
        <AlertTriangle className="size-4" />
        <AlertTitle>Error: {domain.last_error_type || 'Validation failed'}</AlertTitle>
        <AlertDescription>
          {domain.last_error ||
            'Certificate provisioning failed. Verify your DNS records and try again.'}
        </AlertDescription>
      </Alert>
      <div className="flex flex-wrap gap-2">
        <Button onClick={onRetry} disabled={retrying || !canManage}>
          {retrying ? (
            <>
              <Loader2 className="mr-2 size-4 animate-spin" />
              Retrying…
            </>
          ) : (
            <>
              <RefreshCw className="mr-2 size-4" />
              Retry verification
            </>
          )}
        </Button>
        <Button variant="outline" onClick={onCancel} disabled={!canManage}>
          <XCircle className="mr-2 size-4" />
          Cancel order
        </Button>
      </div>
      {dnsTxtRecords.length > 0 && (
        <div className="space-y-2">
          <p className="text-xs text-muted-foreground">
            Reference — verify {dnsTxtRecords.length > 1 ? 'these records exist' : 'this record exists'} in DNS:
          </p>
          <div className="divide-y divide-gray-950/5 rounded-lg border border-gray-950/10 overflow-hidden">
            {dnsTxtRecords.map((record, index) => (
              <div
                key={`${keyPrefix}-${index}`}
                className="grid grid-cols-[auto_1fr_auto] items-start gap-3 px-4 py-2.5"
              >
                <Badge variant="outline" className="mt-0.5">TXT</Badge>
                <div className="min-w-0 space-y-1">
                  <p className="font-mono text-xs break-all">
                    <span className="text-muted-foreground">Name:</span> {record.name}
                  </p>
                  <p className="font-mono text-xs break-all">
                    <span className="text-muted-foreground">Value:</span> {record.value}
                  </p>
                </div>
                <CopyButton
                  value={record.value}
                  minimal
                  className="size-8 rounded-md"
                />
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  )
}
