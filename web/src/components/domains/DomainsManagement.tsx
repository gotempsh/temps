// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

'use client'

import { PageHeader } from '@/components/layout/PageContainer'

import {
  deleteDomainMutation,
  listIngressHostnamesOptions,
  renewDomainMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { allTunnelsOptions } from '@/lib/tunnel-domains'
import { DomainResponse } from '@/api/client/types.gen'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
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
import { useMutation, useQuery } from '@tanstack/react-query'
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from '@/components/ui/collapsible'
import { Input } from '@/components/ui/input'
import {
  AlertTriangle,
  ArrowUp,
  ArrowDown,
  ArrowUpDown,
  ChevronRight,
  Cloud,
  EllipsisVertical,
  Globe,
  Info,
  Plus,
  RefreshCw,
  Search,
  ShieldCheck,
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
import { StateBadge } from './TunnelHostnames'
import {
  domainRowHref,
  mergeDomainRows,
  tunnelRows,
  type DomainRow,
} from './domain-rows'
import { usePlatformCapabilities } from '@/hooks/usePlatformCapabilities'
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'
import { Link, useNavigate } from 'react-router'
import { DataTable, useUrlBoundText, type DataTableColumn } from '@temps-sdk/ds'
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
  // The query lives in the URL; typing is mirrored locally so a router
  // transition cannot reset the box and drop keystrokes.
  const [searchText, setSearchText] = useUrlBoundText(
    searchQuery,
    onSearchChange
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
      }
    )
  }

  // Names tunnels carry, listed with the certificates (ADR 058 D10).
  // Without settings access these 403 and the list is certificates only.
  const connectorsQuery = useQuery({
    ...allTunnelsOptions(),
    retry: false,
  })
  const hostnamesQuery = useQuery({
    ...listIngressHostnamesOptions(),
    refetchInterval: 15_000,
    retry: false,
  })
  const tunnels = tunnelRows(
    connectorsQuery.data ?? [],
    hostnamesQuery.data ?? [],
    searchQuery
  )
  const rows = mergeDomainRows(domains ?? [], tunnels, page, sort, direction)
  const failingTunnels = tunnels.filter(
    (r) => r.kind === 'tunnel' && r.view.state === 'error'
  ).length
  // Open by default only while there is nothing to list; then it is noise.
  const [dnsTipsOpen, setDnsTipsOpen] = useState<boolean>()

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
  const columns: DataTableColumn<DomainRow>[] = [
    {
      ...sortable('domain', 'Domain'),
      render: (row) => (
        <Link
          to={domainRowHref(row)}
          className="font-medium hover:underline focus-visible:underline"
        >
          {row.name}
        </Link>
      ),
    },
    {
      ...sortable('status', 'Status'),
      render: (row) =>
        row.kind === 'certificate' ? (
          <DomainStatusBadge status={row.domain.status} />
        ) : row.kind === 'tunnel' ? (
          <StateBadge view={row.view} />
        ) : (
          <TunnelZoneStatus row={row} />
        ),
    },
    {
      key: 'https',
      header: 'HTTPS',
      render: (row) => <HttpsSource row={row} />,
    },
    {
      ...sortable('expiration', 'Certificate expires'),
      render: (row) =>
        row.kind === 'certificate' ? (
          <DomainExpiration domain={row.domain} />
        ) : (
          <span className="text-muted-foreground">Renewed by Cloudflare</span>
        ),
    },
    {
      key: 'actions',
      header: <span className="sr-only">Actions</span>,
      className: 'w-12',
      render: (row) =>
        row.kind === 'certificate' ? (
          <DomainRowMenu
            domain={row.domain}
            onOpen={(id) => navigate(`/domains/${id}`)}
            onRenew={handleRenewDomain}
            onDelete={setDomainToDelete}
            canManageCertificates={canManageCertificates}
          />
        ) : (
          <Button
            variant="ghost"
            size="icon"
            className="h-8 w-8"
            aria-label={`Details for ${row.name}`}
            onClick={() => navigate(domainRowHref(row))}
          >
            <ChevronRight className="h-4 w-4" />
          </Button>
        ),
    },
  ]

  return (
    <div className="space-y-6">
      <PageHeader
        title="Domains"
        description="Every domain this install serves, and how each gets HTTPS: its own certificate or a tunnel"
        actions={
          <>
            {' '}
            {canCreateDomains ? (
              <CreateActionButton
                to="/domains/add"
                label="Add domain"
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
      {/* How to point DNS at this server: only certificate domains need it,
          so it waits behind a disclosure instead of leading the page. */}
      <Collapsible
        open={
          dnsTipsOpen ??
          (!isLoading && !connectorsQuery.isLoading && rows.length === 0)
        }
        onOpenChange={setDnsTipsOpen}
      >
        <CollapsibleTrigger className="group inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground">
          <ChevronRight className="h-4 w-4 transition-transform group-data-[state=open]:rotate-90" />
          Pointing DNS at this server
        </CollapsibleTrigger>
        <CollapsibleContent className="mt-3">
          <DNSConfigurationHelper />
        </CollapsibleContent>
      </Collapsible>

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

      {failingTunnels > 0 && (
        <Alert variant="destructive">
          <AlertTriangle className="h-4 w-4" />
          <AlertTitle>
            {failingTunnels} domain{failingTunnels > 1 ? 's' : ''} could not be
            published through a tunnel
          </AlertTitle>
          <AlertDescription>Open one to see why.</AlertDescription>
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
          value={searchText}
          onChange={(e) => setSearchText(e.target.value)}
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

      <ConfirmDeleteDialog
        open={domainToDelete !== null}
        onOpenChange={(open) => {
          if (!open) setDomainToDelete(null)
        }}
        resourceType="domain"
        resourceName={domainToDelete?.domain ?? ''}
        consequences="All TLS certificates issued for this domain are removed with it."
        isPending={deleteDomain.isPending}
        onConfirm={() => {
          if (domainToDelete) handleDeleteDomain(domainToDelete.domain)
        }}
      />

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
      ) : !isLoading && rows.length === 0 ? (
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
                Add domain
              </Button>
            }
          />
        )
      ) : (
        <DataTable
          aria-label="Domains"
          columns={columns}
          rows={rows}
          rowKey={(row) => row.key}
          isLoading={isLoading}
          pagination={
            !isLoading
              ? {
                  page,
                  pageSize,
                  total: total + tunnels.length,
                  totalPages,
                  onPageChange,
                }
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

/** How a domain gets HTTPS: its own certificate, or the tunnel's provider. */
function HttpsSource({ row }: { row: DomainRow }) {
  if (row.kind === 'certificate') {
    const method = row.domain.verification_method
    const how =
      method === 'manual'
        ? 'Uploaded certificate'
        : method === 'dns-01'
          ? "Let's Encrypt · DNS"
          : "Let's Encrypt · HTTP"
    return (
      <span className="inline-flex items-center gap-1.5 text-muted-foreground">
        <ShieldCheck className="h-3.5 w-3.5 shrink-0" />
        {how}
      </span>
    )
  }
  const name =
    row.kind === 'tunnel' ? row.view.connector_name : row.connector.name
  return (
    <span className="inline-flex items-center gap-1.5 text-muted-foreground">
      <Cloud className="h-3.5 w-3.5 shrink-0" />
      {name ? `Tunnel ${name}` : 'No tunnel'}
      {row.kind === 'tunnel_zone' && ' · covers every name'}
    </span>
  )
}

function TunnelZoneStatus({
  row,
}: {
  row: Extract<DomainRow, { kind: 'tunnel_zone' }>
}) {
  const { connector } = row
  const connected = connector.nodes.filter((n) => n.state === 'ready').length
  if (connector.status === 'error') {
    return (
      <Badge variant="destructive" className="text-xs">
        tunnel failed
      </Badge>
    )
  }
  return (
    <Badge variant={connected > 0 ? 'default' : 'warning'} className="text-xs">
      {connected > 0
        ? connector.auto_publish
          ? 'publishing all'
          : 'publishing picked'
        : 'tunnel down'}
    </Badge>
  )
}
