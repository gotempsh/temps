'use client'

import {
  deleteDomainMutation,
  renewDomainMutation,
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
import { CreateActionButton } from '@/components/ui/create-action-button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { EmptyState } from '@/components/ui/empty-state'
import { useMutation } from '@tanstack/react-query'
import { Input } from '@/components/ui/input'
import {
  AlertTriangle,
  ChevronLeft,
  ChevronRight,
  EllipsisVertical,
  Globe,
  Info,
  Plus,
  RefreshCw,
  Search,
  Trash2,
} from 'lucide-react'
import { useState } from 'react'
import { toast } from 'sonner'
import { formatExpiryRemaining, formatLocalDate } from '@/lib/date'
import {
  STATUS_ACTIVE_RENEWAL_FAILED,
  isServingCert,
} from '@/lib/domain-status'
import { DNSConfigurationHelper } from './DNSConfigurationHelper'
import { usePlatformCapabilities } from '@/hooks/usePlatformCapabilities'
import { useNavigate } from 'react-router-dom'

interface DomainsManagementProps {
  domains?: DomainResponse[]
  isLoading: boolean
  reloadDomains: () => void
  total: number
  page: number
  pageSize: number
  totalPages: number
  onPageChange: (page: number) => void
  searchQuery: string
  onSearchChange: (value: string) => void
  isSearching: boolean
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
  total,
  page,
  pageSize,
  totalPages,
  onPageChange,
  searchQuery,
  onSearchChange,
  isSearching,
}: DomainsManagementProps) {
  const [domainToDelete, setDomainToDelete] = useState<DomainResponse | null>(
    null
  )
  const navigate = useNavigate()

  const {
    canManageCertificates,
    canCreateDomains,
    isUsingCloudflare,
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

  const renewDomain = useMutation({
    ...renewDomainMutation(),
    meta: {
      errorTitle: 'Failed to renew domain certificate',
    },
  })

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

  const getPaginationPages = (currentPage: number, total: number) => {
    const pageNumbers = []
    const maxButtons = 5
    let startPage = Math.max(1, currentPage - Math.floor(maxButtons / 2))
    const endPage = Math.min(total, startPage + maxButtons - 1)

    if (endPage - startPage < maxButtons - 1) {
      startPage = Math.max(1, endPage - maxButtons + 1)
    }

    for (let i = startPage; i <= endPage; i++) {
      pageNumbers.push(i)
    }

    return pageNumbers
  }

  const pendingProvisioningCount =
    domains?.filter(
      (domain) =>
        domain.status === 'pending_dns' ||
        domain.status === 'pending' ||
        domain.status === 'failed'
    ).length || 0

  return (
    <div className="space-y-4">
      <DNSConfigurationHelper />

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

      {pendingProvisioningCount > 0 && (
        <Alert className="border-yellow-200 bg-yellow-50/50 dark:bg-yellow-950/10">
          <AlertTriangle className="h-4 w-4 text-yellow-600" />
          <AlertTitle className="flex items-center gap-2">
            <span>TLS Certificates Pending</span>
            <Badge variant="secondary">{pendingProvisioningCount}</Badge>
          </AlertTitle>
          <AlertDescription>
            {pendingProvisioningCount} domain
            {pendingProvisioningCount > 1 ? 's' : ''} require
            {pendingProvisioningCount === 1 ? 's' : ''} TLS certificate
            provisioning or DNS verification.
          </AlertDescription>
        </Alert>
      )}

      <div className="flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between">
        <div>
          <h2 className="text-lg font-semibold">Domains</h2>
          <p className="text-sm text-muted-foreground">
            Manage your custom domains and TLS certificates
          </p>
        </div>
        {canCreateDomains ? (
          <CreateActionButton
            to="/domains/add"
            label="Add Domain"
            icon={<Globe className="h-4 w-4" />}
          />
        ) : (
          <Button disabled>
            <Globe className="mr-2 h-4 w-4" />
            Managed by Cloudflare
          </Button>
        )}
      </div>

      <div className="relative">
        <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4 text-muted-foreground" />
        <Input
          value={searchQuery}
          onChange={(e) => onSearchChange(e.target.value)}
          placeholder="Search domains..."
          className="pl-9 pr-10"
        />
        {isSearching && (
          <div className="absolute right-3 top-1/2 -translate-y-1/2">
            <RefreshCw className="h-4 w-4 animate-spin text-muted-foreground" />
          </div>
        )}
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
              domain and remove all associated TLS certificates.
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

      {isLoading ? (
        <div className="divide-y rounded-lg border">
          {Array.from({ length: 3 }).map((_, i) => (
            <div key={i} className="flex items-center gap-4 px-4 py-3 animate-pulse">
              <div className="size-9 shrink-0 rounded-md bg-muted" />
              <div className="flex-1 min-w-0 space-y-1.5">
                <div className="h-4 w-48 bg-muted rounded" />
                <div className="h-3 w-64 bg-muted rounded" />
              </div>
              <div className="h-6 w-20 bg-muted rounded" />
            </div>
          ))}
        </div>
      ) : !domains?.length ? (
        searchQuery ? (
          <EmptyState
            icon={Search}
            title="No domains match your search"
            description={`No domains found matching "${searchQuery}"`}
            action={
              <Button variant="outline" onClick={() => onSearchChange('')}>
                Clear search
              </Button>
            }
          />
        ) : (
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
        )
      ) : (
        <DomainsCompactRows
          domains={domains}
          onOpen={(id) => navigate(`/domains/${id}`)}
          onRenew={handleRenewDomain}
          onDelete={setDomainToDelete}
          canManageCertificates={canManageCertificates}
        />
      )}

      {totalPages > 1 && (
        <div className="flex flex-col sm:flex-row items-center justify-between gap-4">
          <div className="text-xs sm:text-sm text-muted-foreground text-center sm:text-left">
            Showing {(page - 1) * pageSize + 1} to{' '}
            {Math.min(page * pageSize, total)} of {total} domains
          </div>
          <div className="flex items-center gap-1 sm:gap-2">
            <Button
              variant="outline"
              size="sm"
              onClick={() => onPageChange(Math.max(1, page - 1))}
              disabled={page === 1}
              className="h-8 px-2 sm:h-9 sm:px-3"
            >
              <ChevronLeft className="h-4 w-4" />
              <span className="hidden sm:inline ml-1">Previous</span>
            </Button>
            <div className="hidden sm:flex items-center gap-1">
              {getPaginationPages(page, totalPages).map((pageNum) => (
                <Button
                  key={pageNum}
                  variant={pageNum === page ? 'default' : 'outline'}
                  size="sm"
                  onClick={() => onPageChange(pageNum)}
                  className="w-10"
                >
                  {pageNum}
                </Button>
              ))}
            </div>
            <span className="sm:hidden text-xs text-muted-foreground px-2">
              {page} / {totalPages}
            </span>
            <Button
              variant="outline"
              size="sm"
              onClick={() => onPageChange(Math.min(totalPages, page + 1))}
              disabled={page === totalPages}
              className="h-8 px-2 sm:h-9 sm:px-3"
            >
              <span className="hidden sm:inline mr-1">Next</span>
              <ChevronRight className="h-4 w-4" />
            </Button>
          </div>
        </div>
      )}
    </div>
  )
}

interface DomainsCompactRowsProps {
  domains: DomainResponse[]
  onOpen: (id: number) => void
  onRenew: (domain: string) => void
  onDelete: (domain: DomainResponse) => void
  canManageCertificates: boolean
}

function DomainStatusBadge({ status }: { status: string }) {
  const variant: 'default' | 'secondary' | 'destructive' | 'warning' =
    status === 'active'
      ? 'default'
      : status === STATUS_ACTIVE_RENEWAL_FAILED
        ? 'warning'
        : status === 'failed'
          ? 'destructive'
          : status === 'pending_dns'
            ? 'warning'
            : 'secondary'
  // "active_renewal_failed" is verbose; show a clearer label while keeping the
  // serving state obvious (the cert is still live).
  const label =
    status === STATUS_ACTIVE_RENEWAL_FAILED
      ? 'renewal failed'
      : status.replace('_', ' ')
  return (
    <Badge variant={variant} className="text-xs">
      {label}
    </Badge>
  )
}

function DomainRowMenu({
  domain,
  onOpen,
  onRenew,
  onDelete,
  canManageCertificates,
}: {
  domain: DomainResponse
  onOpen: (id: number) => void
  onRenew: (domain: string) => void
  onDelete: (domain: DomainResponse) => void
  canManageCertificates: boolean
}) {
  return (
    <div
      onClick={(e) => e.stopPropagation()}
      onPointerDown={(e) => e.stopPropagation()}
    >
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button variant="ghost" size="icon" className="h-8 w-8">
            <EllipsisVertical className="h-4 w-4" />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end">
          <DropdownMenuItem
            onSelect={(e) => {
              e.preventDefault()
              onOpen(domain.id)
            }}
          >
            <Globe className="mr-2 h-4 w-4" />
            View details
          </DropdownMenuItem>
          {isServingCert(domain.status) && canManageCertificates && (
            <DropdownMenuItem
              onSelect={(e) => {
                e.preventDefault()
                onRenew(domain.domain)
              }}
            >
              <RefreshCw className="mr-2 h-4 w-4" />
              Renew certificate
            </DropdownMenuItem>
          )}
          <DropdownMenuSeparator />
          <DropdownMenuItem
            className="text-destructive"
            onSelect={(e) => {
              e.preventDefault()
              onDelete(domain)
            }}
          >
            <Trash2 className="mr-2 h-4 w-4" />
            Delete
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  )
}

function DomainsCompactRows({
  domains,
  onOpen,
  onRenew,
  onDelete,
  canManageCertificates,
}: DomainsCompactRowsProps) {
  return (
    <div className="overflow-hidden rounded-lg border">
      <ul role="list" className="divide-y">
        {domains.map((domain) => {
          const expiringSoon =
            isServingCert(domain.status) &&
            isExpiringSoon(domain.expiration_time || 0)
          const expires = domain.expiration_time
            ? formatLocalDate(domain.expiration_time)
            : null
          const remaining = domain.expiration_time
            ? formatExpiryRemaining(domain.expiration_time)
            : null
          const expiryBadgeVariant: 'destructive' | 'warning' =
            remaining?.expired || (remaining && remaining.totalHours < 48)
              ? 'destructive'
              : 'warning'
          const expiryBadgeLabel = remaining
            ? remaining.expired
              ? `Expired ${remaining.short} ago`
              : `Expires in ${remaining.short}`
            : 'Expires soon'
          return (
            <li
              key={domain.id}
              role="button"
              tabIndex={0}
              onClick={() => onOpen(domain.id)}
              onKeyDown={(e) => {
                if (e.key === 'Enter' || e.key === ' ') {
                  e.preventDefault()
                  onOpen(domain.id)
                }
              }}
              className="flex cursor-pointer items-center gap-4 px-4 py-3 hover:bg-muted/40 transition-colors focus:outline-none focus:bg-muted/40"
            >
              <div className="flex size-9 shrink-0 items-center justify-center rounded-md bg-muted">
                <Globe className="size-4 text-muted-foreground" />
              </div>
              <div className="flex min-w-0 flex-1 items-center gap-3">
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-2 flex-wrap">
                    <p className="truncate text-sm font-medium">
                      {domain.domain}
                    </p>
                    <DomainStatusBadge status={domain.status} />
                    {domain.is_wildcard && (
                      <Badge variant="outline" className="text-xs">
                        Wildcard
                      </Badge>
                    )}
                    {expiringSoon && (
                      <Badge variant={expiryBadgeVariant} className="text-xs">
                        {expiryBadgeLabel}
                      </Badge>
                    )}
                  </div>
                  {expires && (
                    <p className="mt-0.5 truncate text-xs text-muted-foreground">
                      Expires {expires}
                    </p>
                  )}
                </div>
              </div>
              <DomainRowMenu
                domain={domain}
                onOpen={onOpen}
                onRenew={onRenew}
                onDelete={onDelete}
                canManageCertificates={canManageCertificates}
              />
              <ChevronRight className="size-4 shrink-0 text-muted-foreground/50" />
            </li>
          )
        })}
      </ul>
    </div>
  )
}
