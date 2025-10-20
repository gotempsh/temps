import { useQuery } from '@tanstack/react-query'
import {
  listDomainsOptions,
  provisionDomainMutation,
  finalizeOrderMutation,
  cancelDomainOrderMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { DomainResponse } from '@/api/client/types.gen'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import { EmptyState } from '@/components/ui/empty-state'
import { Spinner } from '@/components/ui/spinner'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { usePlatformCapabilities } from '@/hooks/usePlatformCapabilities'
import { useMutation } from '@tanstack/react-query'
import {
  AlertTriangle,
  CheckCircle,
  Clock,
  CopyIcon,
  Globe,
  Info,
  Loader2,
  RefreshCw,
  Shield,
  XCircle,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { toast } from 'sonner'
import { useNavigate } from 'react-router-dom'
import { DNSConfigurationHelper } from '@/components/domains/DNSConfigurationHelper'

export function DomainProvisioning() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const navigate = useNavigate()
  const {
    data: domains,
    isLoading,
    refetch,
  } = useQuery({
    ...listDomainsOptions({}),
  })

  const { canManageCertificates, isUsingCloudflare } = usePlatformCapabilities()

  const [isCompletingDns, setIsCompletingDns] = useState<string | null>(null)

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Domains', href: '/domains' },
      { label: 'SSL Provisioning' },
    ])
  }, [setBreadcrumbs])

  usePageTitle('SSL Certificate Provisioning')

  const provisionDomain = useMutation({
    ...provisionDomainMutation(),
    meta: {
      errorTitle: 'Failed to provision SSL certificate',
    },
    onSuccess: () => {
      toast.success('SSL certificate provisioning started')
      refetch()
    },
  })

  const finalizeOrder = useMutation({
    ...finalizeOrderMutation(),
    meta: {
      errorTitle: 'Failed to verify DNS challenge',
    },
    onSuccess: () => {
      toast.success(
        'DNS challenge verified! SSL certificate provisioning in progress.'
      )
      refetch()
    },
  })

  const cancelOrder = useMutation({
    ...cancelDomainOrderMutation(),
    meta: {
      errorTitle: 'Failed to cancel ACME order',
    },
    onSuccess: () => {
      toast.success(
        'ACME order cancelled successfully. You can now start over.'
      )
      refetch()
    },
  })

  const handleProvisionDomain = async (domainName: string) => {
    toast.promise(
      provisionDomain.mutateAsync({
        path: {
          domain: domainName,
        },
      }),
      {
        loading: `Provisioning SSL certificate for ${domainName}...`,
        success: () => {
          return `SSL certificate provisioning started for ${domainName}`
        },
        error: `Failed to provision SSL certificate for ${domainName}`,
      }
    )
  }

  const handleCompleteDns = async (domainId: number) => {
    try {
      setIsCompletingDns(domainId.toString())
      await finalizeOrder.mutateAsync({
        path: {
          domain_id: domainId,
        },
      })
    } catch (error) {
      // Error handled in onError
    } finally {
      setIsCompletingDns(null)
    }
  }

  const handleCancelOrder = async (domainName: string) => {
    toast.promise(
      cancelOrder.mutateAsync({
        path: {
          domain: domainName,
        },
      }),
      {
        loading: `Cancelling ACME order for ${domainName}...`,
        success: () => {
          return `ACME order cancelled for ${domainName}. You can now start fresh.`
        },
        error: `Failed to cancel ACME order for ${domainName}`,
      }
    )
  }

  // Filter domains that need provisioning
  const pendingDomains =
    domains?.domains?.filter(
      (domain) => domain.status === 'pending_dns' || domain.status === 'pending'
    ) || []

  const failedDomains =
    domains?.domains?.filter(
      (domain) => domain.status === 'failed' && domain.last_error
    ) || []

  return (
    <div className="flex-1 overflow-auto">
      <div className="max-w-5xl mx-auto space-y-6">
        {/* Header */}
        <div className="flex flex-col sm:flex-row sm:items-center justify-between gap-4">
          <div>
            <h1 className="text-2xl font-bold">SSL Certificate Provisioning</h1>
            <p className="text-sm text-muted-foreground mt-1">
              Manage SSL certificate provisioning and DNS challenges
            </p>
          </div>
          <Button variant="outline" onClick={() => navigate('/domains')}>
            <Globe className="mr-2 h-4 w-4" />
            Back to Domains
          </Button>
        </div>

        {/* DNS Configuration Helper */}
        <DNSConfigurationHelper />

        {/* Cloudflare mode information */}
        {isUsingCloudflare() && (
          <Alert className="border-purple-200 bg-purple-50/50 dark:bg-purple-950/10">
            <Info className="h-4 w-4 text-purple-600" />
            <AlertDescription>
              Domain and certificate management is handled automatically by
              Cloudflare Tunnel. SSL provisioning is managed through your
              Cloudflare dashboard.
            </AlertDescription>
          </Alert>
        )}

        {/* Pending DNS Challenges */}
        {!isLoading && pendingDomains.length > 0 && (
          <Card>
            <div className="p-6 space-y-4">
              <div className="flex items-center gap-2">
                <Clock className="h-5 w-5 text-yellow-600" />
                <h2 className="text-lg font-semibold">
                  Pending DNS Challenges
                </h2>
                <Badge variant="secondary">{pendingDomains.length}</Badge>
              </div>
              <p className="text-sm text-muted-foreground">
                These domains require DNS verification before SSL certificates
                can be provisioned.
              </p>

              <div className="space-y-4">
                {pendingDomains.map((domain) => (
                  <DomainProvisioningCard
                    key={domain.id}
                    domain={domain}
                    onProvision={handleProvisionDomain}
                    onCompleteDns={handleCompleteDns}
                    onCancelOrder={handleCancelOrder}
                    isCompletingDns={isCompletingDns === domain.id.toString()}
                    canManage={canManageCertificates}
                  />
                ))}
              </div>
            </div>
          </Card>
        )}

        {/* Failed Provisioning */}
        {!isLoading && failedDomains.length > 0 && (
          <Card>
            <div className="p-6 space-y-4">
              <div className="flex items-center gap-2">
                <AlertTriangle className="h-5 w-5 text-destructive" />
                <h2 className="text-lg font-semibold">Failed Provisioning</h2>
                <Badge variant="destructive">{failedDomains.length}</Badge>
              </div>
              <p className="text-sm text-muted-foreground">
                These domains encountered errors during SSL certificate
                provisioning.
              </p>

              <div className="space-y-4">
                {failedDomains.map((domain) => (
                  <FailedDomainCard
                    key={domain.id}
                    domain={domain}
                    onRetry={handleProvisionDomain}
                    onCancelOrder={handleCancelOrder}
                    canManage={canManageCertificates}
                  />
                ))}
              </div>
            </div>
          </Card>
        )}

        {/* Loading state */}
        {isLoading && (
          <Card>
            <div className="p-6">
              <div className="flex items-center justify-center py-8">
                <Loader2 className="h-8 w-8 animate-spin text-muted-foreground" />
              </div>
            </div>
          </Card>
        )}

        {/* Empty state */}
        {!isLoading &&
          pendingDomains.length === 0 &&
          failedDomains.length === 0 && (
            <Card>
              <div className="p-6">
                <EmptyState
                  icon={CheckCircle}
                  title="All certificates provisioned"
                  description="All your domains have active SSL certificates. No provisioning required."
                  action={
                    <Button
                      variant="outline"
                      onClick={() => navigate('/domains')}
                    >
                      <Globe className="mr-2 h-4 w-4" />
                      View All Domains
                    </Button>
                  }
                />
              </div>
            </Card>
          )}
      </div>
    </div>
  )
}

interface DomainProvisioningCardProps {
  domain: DomainResponse
  onProvision: (domainName: string) => void
  onCompleteDns: (domainId: number) => void
  onCancelOrder: (domainName: string) => void
  isCompletingDns: boolean
  canManage: boolean
}

function DomainProvisioningCard({
  domain,
  onProvision,
  onCompleteDns,
  onCancelOrder,
  isCompletingDns,
  canManage,
}: DomainProvisioningCardProps) {
  return (
    <div className="p-4 border rounded-lg space-y-4">
      <div className="flex items-start justify-between gap-4">
        <div className="space-y-1">
          <div className="flex items-center gap-3">
            <span className="font-medium">{domain.domain}</span>
            <Badge variant="secondary">{domain.status}</Badge>
            {domain.is_wildcard && <Badge variant="outline">Wildcard</Badge>}
          </div>
          <p className="text-sm text-muted-foreground">
            {domain.status === 'pending_dns'
              ? 'Waiting for DNS verification'
              : 'Ready for provisioning'}
          </p>
        </div>
      </div>

      {domain.status === 'pending_dns' &&
      domain.dns_challenge_token &&
      domain.dns_challenge_value ? (
        <Alert>
          <Shield className="h-4 w-4" />
          <AlertTitle className="flex items-center justify-between">
            <div className="flex items-center gap-2">
              DNS Verification Required
            </div>
            <Button
              size="sm"
              onClick={() => onProvision(domain.domain)}
              disabled={!canManage}
              variant="outline"
            >
              <RefreshCw className="h-4 w-4 mr-2" />
              {canManage ? 'Check DNS' : 'Managed by Cloudflare'}
            </Button>
          </AlertTitle>
          <AlertDescription>
            <div className="mt-4 space-y-4">
              <p className="text-sm text-muted-foreground">
                Add the following DNS record to verify domain ownership:
              </p>
              <div className="space-y-3">
                <div className="space-y-2">
                  <div className="flex items-center justify-between">
                    <span className="text-sm font-medium">Record Name</span>
                    <Button
                      variant="ghost"
                      size="sm"
                      className="h-8"
                      onClick={() => {
                        navigator.clipboard.writeText(
                          domain.dns_challenge_token || ''
                        )
                        toast.success('Copied to clipboard')
                      }}
                    >
                      <CopyIcon className="h-3 w-3 mr-2" />
                      Copy
                    </Button>
                  </div>
                  <code className="relative block rounded bg-muted px-3 py-2 font-mono text-sm break-all">
                    {domain.dns_challenge_token}
                  </code>
                </div>
                <div className="space-y-2">
                  <div className="flex items-center justify-between">
                    <span className="text-sm font-medium">Record Value</span>
                    <Button
                      variant="ghost"
                      size="sm"
                      className="h-8"
                      onClick={() => {
                        navigator.clipboard.writeText(
                          domain.dns_challenge_value || ''
                        )
                        toast.success('Copied to clipboard')
                      }}
                    >
                      <CopyIcon className="h-3 w-3 mr-2" />
                      Copy
                    </Button>
                  </div>
                  <code className="relative block rounded bg-muted px-3 py-2 font-mono text-sm break-all">
                    {domain.dns_challenge_value}
                  </code>
                </div>
              </div>
              <div className="rounded-lg bg-blue-50 dark:bg-blue-950/20 p-3 space-y-2">
                <p className="text-sm font-medium text-blue-900 dark:text-blue-100">
                  Next Steps:
                </p>
                <ol className="list-decimal list-inside space-y-1 text-sm text-blue-800 dark:text-blue-200">
                  <li>
                    Add the TXT record to your DNS provider (e.g., Cloudflare,
                    Route53, Namecheap)
                  </li>
                  <li>
                    Wait for DNS propagation (can take up to 24 hours, usually
                    5-15 minutes)
                  </li>
                  <li>
                    Click "Complete DNS Challenge" below to verify and provision
                    the certificate
                  </li>
                </ol>
              </div>
              <div className="flex gap-2">
                <Button
                  onClick={() => onCompleteDns(domain.id)}
                  disabled={isCompletingDns || !canManage}
                  className="flex-1"
                >
                  {isCompletingDns ? (
                    <>
                      <Spinner className="mr-2 h-4 w-4" />
                      Completing DNS Challenge...
                    </>
                  ) : (
                    <>
                      <CheckCircle className="mr-2 h-4 w-4" />
                      {canManage
                        ? 'Complete DNS Challenge'
                        : 'Managed by Cloudflare'}
                    </>
                  )}
                </Button>
                <Button
                  variant="outline"
                  onClick={() => onCancelOrder(domain.domain)}
                  disabled={!canManage}
                >
                  <XCircle className="mr-2 h-4 w-4" />
                  Cancel Order
                </Button>
              </div>
            </div>
          </AlertDescription>
        </Alert>
      ) : (
        <div className="flex justify-end">
          <Button
            onClick={() => onProvision(domain.domain)}
            disabled={!canManage}
            size="sm"
          >
            <Shield className="mr-2 h-4 w-4" />
            {canManage ? 'Start Provisioning' : 'Managed by Cloudflare'}
          </Button>
        </div>
      )}
    </div>
  )
}

interface FailedDomainCardProps {
  domain: DomainResponse
  onRetry: (domainName: string) => void
  onCancelOrder: (domainName: string) => void
  canManage: boolean
}

function FailedDomainCard({
  domain,
  onRetry,
  onCancelOrder,
  canManage,
}: FailedDomainCardProps) {
  return (
    <div className="p-4 border border-destructive/50 rounded-lg space-y-3">
      <div className="flex items-start justify-between gap-4">
        <div className="space-y-1 flex-1">
          <div className="flex items-center gap-3">
            <span className="font-medium">{domain.domain}</span>
            <Badge variant="destructive">{domain.status}</Badge>
            {domain.is_wildcard && <Badge variant="outline">Wildcard</Badge>}
          </div>
          {domain.last_error && (
            <Alert variant="warning" className="mt-2">
              <AlertTriangle className="h-4 w-4" />
              <AlertDescription className="flex flex-col gap-1">
                {domain.last_error_type && (
                  <span className="font-medium text-sm">
                    {domain.last_error_type}
                  </span>
                )}
                <span className="text-sm text-muted-foreground">
                  {domain.last_error}
                </span>
              </AlertDescription>
            </Alert>
          )}
        </div>
        <div className="flex gap-2">
          <Button
            onClick={() => onRetry(domain.domain)}
            disabled={!canManage}
            size="sm"
            variant="outline"
          >
            <RefreshCw className="mr-2 h-4 w-4" />
            {canManage ? 'Retry Provisioning' : 'Managed by Cloudflare'}
          </Button>
          <Button
            onClick={() => onCancelOrder(domain.domain)}
            disabled={!canManage}
            size="sm"
            variant="ghost"
          >
            <XCircle className="mr-2 h-4 w-4" />
            Cancel Order
          </Button>
        </div>
      </div>
    </div>
  )
}
