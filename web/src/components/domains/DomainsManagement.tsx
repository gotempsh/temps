'use client'

import {
  deleteDomainMutation,
  provisionDomainMutation,
  renewDomainMutation,
  finalizeOrderMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { DomainResponse } from '@/api/client/types.gen'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { EmptyState } from '@/components/ui/empty-state'
import { useMutation } from '@tanstack/react-query'
import {
  Calendar,
  CheckCircle,
  Clock,
  CopyIcon,
  Globe,
  Loader2,
  MoreHorizontal,
  Plus,
  RefreshCw,
  Trash2,
  AlertTriangle,
  Info,
} from 'lucide-react'
import { Spinner } from '@/components/ui/spinner'
import { useState } from 'react'
import { toast } from 'sonner'
import { formatUTCDate } from '@/lib/date'
import { DNSConfigurationHelper } from './DNSConfigurationHelper'
import { usePlatformCapabilities } from '@/hooks/usePlatformCapabilities'
import { useNavigate } from 'react-router-dom'

interface DomainsManagementProps {
  domains?: DomainResponse[]
  isLoading: boolean
  reloadDomains: () => void
}

const isExpiringSoon = (expirationTime: number) => {
  const expirationDate = new Date(expirationTime)
  const now = new Date()
  const daysUntilExpiration = Math.ceil(
    (expirationDate.getTime() - now.getTime()) / (1000 * 60 * 60 * 24)
  )
  return daysUntilExpiration <= 15
}

export function DomainsManagement({
  domains,
  isLoading,
  reloadDomains,
}: DomainsManagementProps) {
  const [domainToDelete, setDomainToDelete] = useState<DomainResponse | null>(
    null
  )
  const navigate = useNavigate()

  // Get platform capabilities
  const {
    canManageCertificates,
    canCreateDomains,
    isUsingCloudflare,
    getAccessModeWarning: _getAccessModeWarning,
  } = usePlatformCapabilities()

  const deleteDomain = useMutation({
    ...deleteDomainMutation(),
    meta: {
      errorTitle: 'Failed to delete domain',
    },
    onSuccess: () => {
      toast.success('Domain deleted successfully')
      reloadDomains()
    },
  })

  const provisionDomain = useMutation({
    ...provisionDomainMutation(),
    meta: {
      errorTitle: 'Failed to provision domain',
    },
    onSuccess: () => {
      toast.success('Domain provisioning started')
      reloadDomains()
    },
  })

  const renewDomain = useMutation({
    ...renewDomainMutation(),
    meta: {
      errorTitle: 'Failed to renew domain certificate',
    },
  })

  const finalizeOrder = useMutation({
    ...finalizeOrderMutation(),
    meta: {
      errorTitle: 'Failed to finalize DNS challenge',
    },
    onSuccess: () => {
      toast.success(
        'DNS challenge verified! SSL certificate provisioning in progress.'
      )
      reloadDomains()
    },
  })

  const handleCompleteDns = (domainId: number) => {
    finalizeOrder.mutate({
      path: {
        domain_id: domainId,
      },
    })
  }

  // Helper to check if a specific domain is being finalized
  const isDomainBeingFinalized = (domainId: number) => {
    return (
      finalizeOrder.isPending &&
      finalizeOrder.variables?.path?.domain_id === domainId
    )
  }

  const handleDeleteDomain = async (domain: string) => {
    try {
      await deleteDomain.mutateAsync({
        path: {
          domain: domain,
        },
      })
    } finally {
      setDomainToDelete(null)
    }
  }

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

  const handleRenewDomain = async (domainName: string) => {
    toast.promise(
      renewDomain.mutateAsync({
        path: {
          domain: domainName,
        },
      }),
      {
        loading: `Renewing ${domainName}...`,
        success: () => {
          reloadDomains()
          return `${domainName} renewed successfully`
        },
        error: `Failed to renew ${domainName}`,
      }
    )
  }

  // Count pending provisioning domains
  const pendingProvisioningCount =
    domains?.filter(
      (domain) =>
        domain.status === 'pending_dns' ||
        domain.status === 'pending' ||
        domain.status === 'failed'
    ).length || 0

  return (
    <div className="space-y-4">
      {/* DNS Configuration Helper - shows IP and instructions based on platform mode */}
      <DNSConfigurationHelper />

      {/* Cloudflare mode information */}
      {isUsingCloudflare() && (
        <Alert className="border-purple-200 bg-purple-50/50 dark:bg-purple-950/10">
          <Info className="h-4 w-4 text-purple-600" />
          <AlertDescription>
            Domain and certificate management is handled automatically by
            Cloudflare Tunnel. Add or remove domains through your Cloudflare
            dashboard.
          </AlertDescription>
        </Alert>
      )}

      {/* SSL Provisioning Alert */}
      {pendingProvisioningCount > 0 && (
        <Alert className="border-yellow-200 bg-yellow-50/50 dark:bg-yellow-950/10">
          <AlertTriangle className="h-4 w-4 text-yellow-600" />
          <AlertTitle className="flex items-center gap-2">
            <span>SSL Certificates Pending</span>
            <Badge variant="secondary">{pendingProvisioningCount}</Badge>
          </AlertTitle>
          <AlertDescription>
            {pendingProvisioningCount} domain
            {pendingProvisioningCount > 1 ? 's' : ''} require
            {pendingProvisioningCount === 1 ? 's' : ''} SSL certificate
            provisioning or DNS verification.
          </AlertDescription>
        </Alert>
      )}

      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-lg font-semibold">Domains</h2>
          <p className="text-sm text-muted-foreground">
            Manage your custom domains and SSL certificates
          </p>
        </div>
        <Button
          disabled={!canCreateDomains}
          onClick={() => navigate('/domains/add')}
        >
          <Globe className="mr-2 h-4 w-4" />
          {canCreateDomains ? 'Add Domain' : 'Managed by Cloudflare'}
        </Button>
      </div>

      <AlertDialog
        open={domainToDelete !== null}
        onOpenChange={(open) => !open && setDomainToDelete(null)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Are you sure?</AlertDialogTitle>
            <AlertDialogDescription>
              This action cannot be undone. This will permanently delete the
              domain and remove all associated SSL certificates.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              onClick={() =>
                domainToDelete && handleDeleteDomain(domainToDelete.domain)
              }
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
              disabled={deleteDomain.isPending}
            >
              {deleteDomain.isPending ? 'Deleting...' : 'Delete'}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      <Card>
        <div className="p-6">
          {isLoading ? (
            <div className="grid gap-4">
              {Array.from({ length: 3 }).map((_, i) => (
                <div
                  key={i}
                  className="p-4 border rounded-lg space-y-3 animate-pulse"
                >
                  <div className="flex items-center justify-between">
                    <div className="h-5 w-48 bg-muted rounded" />
                    <div className="h-6 w-20 bg-muted rounded" />
                  </div>
                  <div className="grid grid-cols-2 gap-4">
                    <div className="space-y-2">
                      <div className="h-4 w-24 bg-muted rounded" />
                      <div className="h-4 w-32 bg-muted rounded" />
                    </div>
                    <div className="space-y-2">
                      <div className="h-4 w-24 bg-muted rounded" />
                      <div className="h-4 w-32 bg-muted rounded" />
                    </div>
                  </div>
                </div>
              ))}
            </div>
          ) : !domains?.length ? (
            <EmptyState
              icon={Globe}
              title="No domains found"
              description="Get started by adding a custom domain"
              action={
                <Button onClick={() => navigate('/domains/add')}>
                  <Plus className="mr-2 h-4 w-4" />
                  Add Domain
                </Button>
              }
            />
          ) : (
            <div className="grid gap-4">
              {domains.map((domain) => (
                <div
                  key={domain.id}
                  className="group relative p-4 border rounded-lg hover:bg-muted/50 transition-colors"
                >
                  <div className="flex flex-col sm:flex-row sm:items-center gap-4">
                    <div className="flex-1 min-w-0 space-y-1">
                      <div className="flex items-center gap-3">
                        <button
                          onClick={() => navigate(`/domains/${domain.id}`)}
                          className="font-medium truncate hover:underline text-left"
                        >
                          {domain.domain}
                        </button>
                        <Badge
                          variant={
                            domain.status === 'active' ? 'default' : 'secondary'
                          }
                        >
                          {domain.status}
                        </Badge>
                        {domain.is_wildcard && (
                          <Badge variant="outline">Wildcard</Badge>
                        )}
                        {domain.status === 'pending_dns' && (
                          <Badge variant="secondary" className="text-xs">
                            DNS Pending
                          </Badge>
                        )}
                      </div>

                      {domain.status === 'pending_dns' &&
                        domain.dns_challenge_token &&
                        domain.dns_challenge_value && (
                          <Alert className="mt-4">
                            <AlertTitle className="flex items-center justify-between">
                              <div className="flex items-center gap-2">
                                DNS Verification Required
                              </div>
                              <Button
                                size="sm"
                                onClick={() =>
                                  handleProvisionDomain(domain.domain)
                                }
                                disabled={
                                  provisionDomain.isPending ||
                                  !canManageCertificates
                                }
                              >
                                {provisionDomain.isPending ? (
                                  <Loader2 className="h-4 w-4 mr-2 animate-spin" />
                                ) : (
                                  <CheckCircle className="h-4 w-4 mr-2" />
                                )}
                                {canManageCertificates
                                  ? 'Check DNS'
                                  : 'Managed by Cloudflare'}
                              </Button>
                            </AlertTitle>
                            <AlertDescription>
                              <div className="mt-2 space-y-4">
                                <p className="text-sm text-muted-foreground">
                                  Add the following DNS record to verify domain
                                  ownership:
                                </p>
                                <div className="space-y-3">
                                  <div className="space-y-2">
                                    <div className="flex items-center justify-between">
                                      <span className="text-sm font-medium">
                                        Record Name
                                      </span>
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
                                    <code className="relative rounded bg-muted px-[0.3rem] py-[0.2rem] font-mono text-sm">
                                      {domain.dns_challenge_token}
                                    </code>
                                  </div>
                                  <div className="space-y-2">
                                    <div className="flex items-center justify-between">
                                      <span className="text-sm font-medium">
                                        Record Value
                                      </span>
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
                                    <code className="relative rounded bg-muted px-[0.3rem] py-[0.2rem] font-mono text-sm">
                                      {domain.dns_challenge_value}
                                    </code>
                                  </div>
                                </div>
                                <p className="text-sm text-muted-foreground">
                                  DNS changes can take up to 24 hours to
                                  propagate. Once you&apos;ve added the DNS
                                  records, click &quot;Complete DNS
                                  Challenge&quot; to verify and provision the
                                  SSL certificate.
                                </p>
                                <div className="mt-4">
                                  <Button
                                    onClick={() => handleCompleteDns(domain.id)}
                                    disabled={
                                      isDomainBeingFinalized(domain.id) ||
                                      !canManageCertificates
                                    }
                                    className="w-full sm:w-auto"
                                  >
                                    {isDomainBeingFinalized(domain.id) ? (
                                      <>
                                        <Spinner className="mr-2 h-4 w-4" />
                                        Completing DNS Challenge...
                                      </>
                                    ) : (
                                      <>
                                        <CheckCircle className="mr-2 h-4 w-4" />
                                        {canManageCertificates
                                          ? 'Complete DNS Challenge'
                                          : 'Managed by Cloudflare'}
                                      </>
                                    )}
                                  </Button>
                                </div>
                              </div>
                            </AlertDescription>
                          </Alert>
                        )}

                      {domain.status === 'active' &&
                        isExpiringSoon(domain.expiration_time || 0) && (
                          <Alert variant="warning" className="mt-4">
                            <AlertTriangle className="h-4 w-4" />
                            <AlertTitle>Certificate Expiring Soon</AlertTitle>
                            <AlertDescription>
                              The SSL certificate for this domain will expire on{' '}
                              {formatUTCDate(domain.expiration_time || 0)}.
                              Please renew it before expiration to avoid service
                              interruption.
                            </AlertDescription>
                          </Alert>
                        )}

                      {domain.last_error && (
                        <Alert variant="warning" className="mt-4">
                          <AlertTriangle className="h-4 w-4" />
                          <AlertDescription className="flex items-center gap-2">
                            <span className="font-medium">
                              {domain.last_error_type}
                            </span>
                            <span className="text-muted-foreground">
                              {domain.last_error}
                            </span>
                          </AlertDescription>
                        </Alert>
                      )}

                      <div className="grid grid-cols-2 sm:flex sm:items-center gap-x-6 gap-y-1 text-sm text-muted-foreground">
                        {domain.status === 'active' && (
                          <>
                            <div className="flex items-center gap-2">
                              <Clock className="h-4 w-4" />
                              <span>
                                Renewed{' '}
                                {formatUTCDate(domain.last_renewed || 0)}
                              </span>
                            </div>
                            <div className="flex items-center gap-2">
                              <Calendar className="h-4 w-4" />
                              <span>
                                Expires{' '}
                                {formatUTCDate(domain.expiration_time || 0)}
                              </span>
                            </div>
                          </>
                        )}
                      </div>
                    </div>
                    <div className="flex items-center justify-end sm:justify-start gap-2">
                      <DropdownMenu>
                        <DropdownMenuTrigger asChild>
                          <Button variant="ghost" size="icon">
                            <MoreHorizontal className="h-4 w-4" />
                          </Button>
                        </DropdownMenuTrigger>
                        <DropdownMenuContent align="end">
                          {domain.status === 'active' &&
                            canManageCertificates && (
                              <DropdownMenuItem
                                onClick={() => handleRenewDomain(domain.domain)}
                                disabled={
                                  renewDomain.isPending ||
                                  !canManageCertificates
                                }
                              >
                                <RefreshCw className="mr-2 h-4 w-4" />
                                {canManageCertificates
                                  ? 'Renew Certificate'
                                  : 'Managed by Cloudflare'}
                              </DropdownMenuItem>
                            )}
                          <DropdownMenuItem
                            onClick={() => setDomainToDelete(domain)}
                            disabled={deleteDomain.isPending}
                            className="text-destructive"
                          >
                            <Trash2 className="mr-2 h-4 w-4" />
                            Delete
                          </DropdownMenuItem>
                        </DropdownMenuContent>
                      </DropdownMenu>
                    </div>
                  </div>
                </div>
              ))}
            </div>
          )}
        </div>
      </Card>
    </div>
  )
}
