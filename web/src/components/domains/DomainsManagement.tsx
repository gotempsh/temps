// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

'use client'

import { PageHeader } from '@/components/layout/PageContainer'

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
  ArrowUp,
  ArrowDown,
  ArrowUpDown,
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
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'
import { Link, useNavigate } from 'react-router'
import { DataTable, type DataTableColumn } from '@temps-sdk/ds'
import type { DomainSort } from './domain-sort'
import { hasUrgentCertificate } from './domain-expiry'

interface DomainsManagementProps {
  domains?: DomainResponse[]
  sort: DomainSort
  direction: 'asc' | 'desc'
  onSortChange: (sort: DomainSort) => void
  isError: boolean
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

export function DomainsManagement({
  domains,
  sort,
  direction,
  onSortChange,
  isError,
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
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()

  const { canManageCertificates, canCreateDomains, isUsingCloudflare } =
    usePlatformCapabilities()

  const deleteDomain = useMutation({
    ...deleteDomainMutation(),
    meta: {
      errorTitle: 'Failed to delete domain',
    },
    onSuccess: () => {
      toast.success('Domain deleted successfully')
      setDomainToDelete(null)
      reloadDomains()
    },
    onError: (error, variables) => {
      if (
        handleSensitiveActionError(error, () => deleteDomain.mutate(variables))
      ) {
        setDomainToDelete(null)
        return
      }
      const problem = error as { detail?: string; message?: string }
      toast.error(
        problem.detail || problem.message || 'Failed to delete domain'
      )
      setDomainToDelete(null)
    },
  })

  const renewDomain = useMutation({
    ...renewDomainMutation(),
    meta: {
      errorTitle: 'Failed to renew domain certificate',
    },
  })

  const handleDeleteDomain = (domain: string) => {
    deleteDomain.mutate({
      path: {
        domain: domain,
      },
    })
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

  const pendingProvisioningCount =
    domains?.filter(
      (domain) =>
        domain.status === 'pending_dns' ||
        domain.status === 'pending' ||
        domain.status === 'failed'
    ).length || 0

  const sortable = (key: DomainSort, label: string) => ({
    key,
    ariaSort:
      sort === key
        ? direction === 'asc'
          ? ('ascending' as const)
          : ('descending' as const)
        : ('none' as const),
    header: (
      <Button
        variant="ghost"
        size="sm"
        className="-ml-3 gap-2"
        onClick={() => onSortChange(key)}
      >
        {label}
        {sort === key ? (
          direction === 'asc' ? (
            <ArrowUp className="size-3.5" />
          ) : (
            <ArrowDown className="size-3.5" />
          )
        ) : (
          <ArrowUpDown className="size-3.5" />
        )}
      </Button>
    ),
  })
  const columns: DataTableColumn<DomainResponse>[] = [
    {
      ...sortable('domain', 'Domain'),
      render: (domain) => (
        <Link
          to={`/domains/${domain.id}`}
          className="font-medium hover:underline focus-visible:underline"
        >
          {domain.domain}
        </Link>
      ),
    },
    {
      ...sortable('status', 'Status'),
      render: (domain) => <DomainStatusBadge status={domain.status} />,
    },
    {
      key: 'type',
      header: 'Type',
      render: (domain) => (
        <span className="text-muted-foreground">
          {domain.is_wildcard ? 'Wildcard' : 'Single domain'}
        </span>
      ),
    },
    {
      ...sortable('expiration', 'Certificate expires'),
      render: (domain) => <DomainExpiration domain={domain} />,
    },
    {
      key: 'actions',
      header: <span className="sr-only">Actions</span>,
      className: 'w-12',
      render: (domain) => (
        <DomainRowMenu
          domain={domain}
          onOpen={(id) => navigate(`/domains/${id}`)}
          onRenew={handleRenewDomain}
          onDelete={setDomainToDelete}
          canManageCertificates={canManageCertificates}
        />
      ),
    },
  ]

  return (
    <div className="space-y-6">
      <PageHeader
        title="Domains"
        description="Manage your custom domains and TLS certificates"
        actions={
          <>
            {' '}
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
            )}{' '}
          </>
        }
      />
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

      <div className="relative">
        <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4 text-muted-foreground" />
        <Input
          value={searchQuery}
          onChange={(e) => onSearchChange(e.target.value)}
          aria-label="Search domains"
          placeholder="Search domains..."
          className="pl-9 pr-10"
        />
        {isSearching && (
          <div className="absolute right-3 top-1/2 -translate-y-1/2">
            <RefreshCw className="h-4 w-4 animate-spin text-muted-foreground" />
          </div>
        )}
      </div>

      {verificationDialog}

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

      {isError ? (
        <EmptyState
          icon={AlertTriangle}
          title="Could not load domains"
          description="Try loading the domain list again."
          action={
            <Button variant="outline" onClick={reloadDomains}>
              Retry
            </Button>
          }
        />
      ) : !isLoading && !domains?.length ? (
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
        <DataTable
          aria-label="Domains"
          columns={columns}
          rows={domains ?? []}
          rowKey={(domain) => domain.id}
          isLoading={isLoading}
          pagination={
            !isLoading
              ? { page, pageSize, total, totalPages, onPageChange }
              : undefined
          }
        />
      )}
    </div>
  )
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
          <Button
            variant="ghost"
            size="icon"
            className="h-8 w-8"
            aria-label={`Actions for ${domain.domain}`}
          >
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

function DomainExpiration({ domain }: { domain: DomainResponse }) {
  if (
    domain.expiration_time == null ||
    !Number.isFinite(domain.expiration_time)
  ) {
    return <span className="text-muted-foreground">Not available</span>
  }
  const remaining = formatExpiryRemaining(domain.expiration_time)
  const urgent = hasUrgentCertificate(domain.status, domain.expiration_time)
  return (
    <div className="flex flex-wrap items-center gap-2 whitespace-nowrap">
      <span className="tabular-nums">
        {formatLocalDate(domain.expiration_time)}
      </span>
      {urgent && remaining && (
        <Badge
          variant={
            remaining.expired || remaining.totalHours < 48
              ? 'destructive'
              : 'warning'
          }
        >
          {remaining.expired
            ? `Expired ${remaining.short} ago`
            : `In ${remaining.short}`}
        </Badge>
      )}
    </div>
  )
}
