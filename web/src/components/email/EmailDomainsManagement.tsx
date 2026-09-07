// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

'use client'

import {
  deleteEmailDomain as deleteEmailDomainSdk,
  listEmailDomains as listEmailDomainsSdk,
  listEmailProviders as listEmailProvidersSdk,
  verifyDomain,
  type EmailDomainResponse,
  type EmailDomainWithDnsResponse,
  type EmailProviderResponse,
} from '@/api/client'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { CreateActionButton } from '@/components/ui/create-action-button'
import { CopyButton } from '@/components/ui/copy-button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { EmailProviderLogo, type EmailProviderType } from '@/components/ui/email-provider-logo'
import { EmptyState } from '@/components/ui/empty-state'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { cn } from '@/lib/utils'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { formatDistanceToNow } from 'date-fns'
import {
  AlertCircle,
  CheckCircle2,
  ChevronRight,
  Clock,
  EllipsisVertical,
  Globe,
  HelpCircle,
  RefreshCw,
} from 'lucide-react'
import { useMemo } from 'react'
import { useNavigate } from 'react-router'
import { toast } from 'sonner'
import { problemMessage } from './sharedUtils'

// Types
type DnsRecordStatus = 'unknown' | 'verified' | 'pending' | 'failed'
type EmailDomain = EmailDomainResponse
type EmailDomainWithDns = EmailDomainWithDnsResponse
type EmailProvider = EmailProviderResponse
type DnsRecord = EmailDomainWithDnsResponse['dns_records'][number]

// API functions
async function listEmailDomains(): Promise<EmailDomain[]> {
  const response = await listEmailDomainsSdk()
  if (response.error) {
    throw new Error(problemMessage(response.error, 'Failed to fetch email domains'))
  }
  return response.data ?? []
}

async function verifyEmailDomain(id: number): Promise<EmailDomainWithDns> {
  const response = await verifyDomain({ path: { id } })
  if (response.error || !response.data) {
    throw new Error(problemMessage(response.error, 'Failed to verify email domain'))
  }
  return response.data
}

async function deleteEmailDomain(id: number): Promise<void> {
  const response = await deleteEmailDomainSdk({ path: { id } })
  if (response.error) {
    throw new Error(problemMessage(response.error, 'Failed to delete email domain'))
  }
}

async function listEmailProviders(): Promise<EmailProvider[]> {
  const response = await listEmailProvidersSdk()
  if (response.error) {
    throw new Error(problemMessage(response.error, 'Failed to fetch email providers'))
  }
  return response.data ?? []
}

// Status dot — small color-coded indicator that mirrors the Storage page's
// HealthDot, so it sits cleanly on the bottom-right of the provider-logo tile.
function StatusDot({ status, className }: { status: string; className?: string }) {
  const tone =
    status === 'verified'
      ? 'bg-green-500'
      : status === 'pending'
        ? 'bg-amber-500'
        : status === 'failed'
          ? 'bg-red-500'
          : 'bg-muted-foreground/40'
  const label =
    status === 'verified'
      ? 'Verified'
      : status === 'pending'
        ? 'Pending'
        : status === 'failed'
          ? 'Failed'
          : 'Unknown'
  return (
    <span
      className={cn(
        'inline-block size-2 rounded-full ring-2 ring-background',
        tone,
        className
      )}
      title={label}
      aria-label={label}
    />
  )
}

// Inline status dot + label for the sub-row — matches the Storage page's
// ServiceStatusDot visual so status reads the same across the app.
function DomainStatusInline({ status }: { status: string }) {
  const tone =
    status === 'verified'
      ? 'bg-success'
      : status === 'failed'
        ? 'bg-destructive'
        : status === 'pending'
          ? 'bg-warning'
          : 'bg-muted-foreground/40'
  return (
    <span className="flex items-center gap-1.5">
      <span className={cn('size-1.5 rounded-full', tone)} />
      <span className="text-xs capitalize text-muted-foreground">
        {status || 'unknown'}
      </span>
    </span>
  )
}

export function StatusPill({ status }: { status: string }) {
  switch (status) {
    case 'verified':
      return (
        <span className="inline-flex items-center gap-1.5 rounded-md bg-emerald-500/10 px-2 py-1 text-xs font-medium text-emerald-700 ring-1 ring-inset ring-emerald-500/20 dark:text-emerald-400">
          <CheckCircle2 className="size-3.5" />
          Verified
        </span>
      )
    case 'pending':
      return (
        <span className="inline-flex items-center gap-1.5 rounded-md bg-amber-500/10 px-2 py-1 text-xs font-medium text-amber-700 ring-1 ring-inset ring-amber-500/20 dark:text-amber-400">
          <Clock className="size-3.5" />
          Pending
        </span>
      )
    case 'failed':
      return (
        <span className="inline-flex items-center gap-1.5 rounded-md bg-red-500/10 px-2 py-1 text-xs font-medium text-red-700 ring-1 ring-inset ring-red-500/20 dark:text-red-400">
          <AlertCircle className="size-3.5" />
          Failed
        </span>
      )
    default:
      return (
        <span className="inline-flex items-center gap-1.5 rounded-md bg-muted px-2 py-1 text-xs font-medium text-muted-foreground ring-1 ring-inset ring-border">
          {status}
        </span>
      )
  }
}

function DnsRecordStatusBadge({ status }: { status?: DnsRecordStatus }) {
  switch (status) {
    case 'verified':
      return (
        <div className="flex items-center gap-1.5 text-emerald-600 dark:text-emerald-500">
          <CheckCircle2 className="size-4" />
          <span className="text-xs font-medium">Verified</span>
        </div>
      )
    case 'pending':
      return (
        <div className="flex items-center gap-1.5 text-amber-600 dark:text-amber-500">
          <Clock className="size-4" />
          <span className="text-xs font-medium">Pending</span>
        </div>
      )
    case 'failed':
      return (
        <div className="flex items-center gap-1.5 text-destructive">
          <AlertCircle className="size-4" />
          <span className="text-xs font-medium">Failed</span>
        </div>
      )
    case 'unknown':
    default:
      return (
        <div className="flex items-center gap-1.5 text-muted-foreground">
          <HelpCircle className="size-4" />
          <span className="text-xs font-medium">Unknown</span>
        </div>
      )
  }
}

export function DnsVerificationSummary({ records }: { records: DnsRecord[] }) {
  // MX and DMARC are both excluded from the "required" count, matching the
  // backend's are_all_records_verified gate exactly: MX is a deliverability
  // aid rather than a sending/auth prerequisite, and DMARC is a plain TXT
  // record the domain owner sets independently (no provider API manages it),
  // so requiring it here would report a domain as unverified even though the
  // backend already considers SPF+DKIM sufficient and marked it "verified".
  // Both rows still appear in DnsRecordsTable with their real status.
  const requiredRecords = records.filter(
    r => r.record_type !== 'MX' && !r.name.startsWith('_dmarc.')
  )
  const verifiedCount = requiredRecords.filter(r => r.status === 'verified').length
  const pendingCount = requiredRecords.filter(r => r.status === 'pending').length
  const failedCount = requiredRecords.filter(r => r.status === 'failed').length
  const unknownCount = requiredRecords.filter(r => !r.status || r.status === 'unknown').length
  const totalCount = requiredRecords.length

  const allVerified = verifiedCount === totalCount && totalCount > 0

  // Nothing to summarize for domains with no DNS records to manage (e.g.
  // an SMTP-imported domain) — the empty state below already explains that.
  if (totalCount === 0) {
    return null
  }

  if (allVerified) {
    return (
      <div className="flex items-center gap-2 rounded-lg border border-emerald-200 bg-emerald-50 p-3 dark:border-emerald-900 dark:bg-emerald-950/30">
        <CheckCircle2 className="size-5 text-emerald-600 dark:text-emerald-500" />
        <span className="text-sm font-medium text-emerald-700 dark:text-emerald-400">
          All {totalCount} required DNS records verified successfully
        </span>
      </div>
    )
  }

  return (
    <div className="flex flex-wrap items-center gap-x-4 gap-y-2 rounded-lg border bg-muted/40 p-3">
      <span className="text-sm font-medium">DNS status</span>
      {verifiedCount > 0 && (
        <div className="flex items-center gap-1 text-sm text-emerald-600 dark:text-emerald-500">
          <CheckCircle2 className="size-4" />
          <span>{verifiedCount} verified</span>
        </div>
      )}
      {pendingCount > 0 && (
        <div className="flex items-center gap-1 text-sm text-amber-600 dark:text-amber-500">
          <Clock className="size-4" />
          <span>{pendingCount} pending</span>
        </div>
      )}
      {failedCount > 0 && (
        <div className="flex items-center gap-1 text-sm text-destructive">
          <AlertCircle className="size-4" />
          <span>{failedCount} failed</span>
        </div>
      )}
      {unknownCount > 0 && (
        <div className="flex items-center gap-1 text-sm text-muted-foreground">
          <HelpCircle className="size-4" />
          <span>{unknownCount} unknown</span>
        </div>
      )}
    </div>
  )
}

export function DnsRecordsTable({ records }: { records: DnsRecord[] }) {
  if (!records || records.length === 0) {
    return (
      <div className="rounded-md border border-dashed p-4 text-sm text-muted-foreground">
        No DNS records to manage for this domain.
      </div>
    )
  }

  // Deterministic order regardless of what the API returns: TXT records
  // first (sorted by value), then everything else (e.g. MX).
  const sortedRecords = [...records].sort((a, b) => {
    const aIsTxt = a.record_type === 'TXT' ? 0 : 1
    const bIsTxt = b.record_type === 'TXT' ? 0 : 1
    if (aIsTxt !== bIsTxt) return aIsTxt - bIsTxt
    return a.value.localeCompare(b.value)
  })

  return (
    <div className="-mx-6 sm:mx-0 sm:rounded-md sm:border">
      <div className="px-6 sm:px-0">
        <Table className="table-fixed">
          <TableHeader>
            <TableRow>
              <TableHead className="w-[64px]">Type</TableHead>
              <TableHead className="w-[130px] sm:w-[220px]">Name</TableHead>
              <TableHead>Value</TableHead>
              <TableHead className="hidden w-[70px] sm:table-cell">
                Priority
              </TableHead>
              <TableHead className="w-[90px] sm:w-[110px]">Status</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {sortedRecords.map((record, index) => (
              <TableRow
                key={index}
                className={
                  record.status === 'verified'
                    ? 'bg-emerald-50/50 dark:bg-emerald-950/20'
                    : record.status === 'failed'
                      ? 'bg-red-50/50 dark:bg-red-950/20'
                      : ''
                }
              >
                <TableCell>
                  <div className="flex flex-wrap items-center gap-1">
                    <Badge variant="outline">{record.record_type}</Badge>
                    {record.record_type === 'MX' && (
                      <span className="rounded px-1 py-0.5 text-[10px] font-medium text-muted-foreground ring-1 ring-inset ring-border">
                        Optional
                      </span>
                    )}
                  </div>
                </TableCell>
                <TableCell>
                  <div className="flex min-w-0 items-center gap-2">
                    <span
                      className="truncate font-mono text-xs"
                      title={record.name}
                    >
                      {record.name}
                    </span>
                    <CopyButton
                      value={record.name}
                      className="h-6 w-6 shrink-0 rounded-md p-0 hover:bg-accent hover:text-accent-foreground"
                    />
                  </div>
                </TableCell>
                <TableCell>
                  <div className="flex min-w-0 items-center gap-2">
                    <span
                      className="truncate font-mono text-xs"
                      title={record.value}
                    >
                      {record.value}
                    </span>
                    <CopyButton
                      value={record.value}
                      className="h-6 w-6 shrink-0 rounded-md p-0 hover:bg-accent hover:text-accent-foreground"
                    />
                  </div>
                </TableCell>
                <TableCell className="hidden sm:table-cell">
                  {record.priority ?? '-'}
                </TableCell>
                <TableCell>
                  <DnsRecordStatusBadge status={record.status} />
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </div>
    </div>
  )
}

function LoadingSkeleton() {
  return (
    <div className="space-y-4">
      <div className="flex items-center justify-end">
        <Skeleton className="h-9 w-28" />
      </div>
      <div className="overflow-hidden rounded-lg border">
        <ul role="list" className="divide-y">
          {[1, 2, 3].map((i) => (
            <li key={i} className="flex items-center gap-4 px-4 py-3">
              <Skeleton className="size-9 shrink-0 rounded-md" />
              <div className="min-w-0 flex-1 space-y-2">
                <Skeleton className="h-4 w-48" />
                <Skeleton className="h-3 w-32" />
              </div>
              <Skeleton className="size-8 rounded-md" />
            </li>
          ))}
        </ul>
      </div>
    </div>
  )
}

export function EmailDomainsManagement() {
  const queryClient = useQueryClient()
  const navigate = useNavigate()

  const { data: domains, isLoading: isLoadingDomains } = useQuery({
    queryKey: ['email-domains'],
    queryFn: listEmailDomains,
  })

  const { data: providers, isLoading: isLoadingProviders } = useQuery({
    queryKey: ['email-providers'],
    queryFn: listEmailProviders,
  })

  const verifyMutation = useMutation({
    mutationFn: verifyEmailDomain,
    onSuccess: (data) => {
      // MX and DMARC are both excluded — see DnsVerificationSummary's comment.
      // Exclude them from the counts shown here too, so the toast reflects
      // the records that actually gate the domain status.
      const required = data.dns_records.filter(
        r => r.record_type !== 'MX' && !r.name.startsWith('_dmarc.')
      )
      const verifiedCount = required.filter(r => r.status === 'verified').length
      const totalCount = required.length
      const pendingCount = required.filter(r => r.status === 'pending').length
      const failedCount = required.filter(r => r.status === 'failed').length

      if (data.domain.status === 'verified') {
        toast.success('Domain verified', {
          description: `All ${totalCount} required DNS records are properly configured.`,
        })
      } else if (failedCount > 0) {
        toast.error('Some DNS records failed verification', {
          description: `${failedCount} of ${totalCount} required records failed.`,
        })
      } else if (pendingCount > 0) {
        toast.warning('Verification in progress', {
          description: `${verifiedCount} of ${totalCount} required records verified. DNS propagation can take up to 48 hours.`,
        })
      } else {
        toast.info('Verification status updated', {
          description: `${verifiedCount} of ${totalCount} required records verified.`,
        })
      }

      queryClient.setQueryData(['email-domain', data.domain.id], data)
      queryClient.setQueryData(['email-domains'], (oldDomains: EmailDomain[] | undefined) => {
        if (!oldDomains) return oldDomains
        return oldDomains.map((d) =>
          d.id === data.domain.id ? data.domain : d
        )
      })
    },
    onError: (error: Error) => {
      toast.error('Failed to verify domain', { description: error.message })
    },
  })

  const deleteMutation = useMutation({
    mutationFn: deleteEmailDomain,
    onSuccess: () => {
      toast.success('Domain deleted')
      queryClient.invalidateQueries({ queryKey: ['email-domains'] })
    },
    onError: (error: Error) => {
      toast.error('Failed to delete domain', { description: error.message })
    },
  })

  const handleVerify = (id: number) => verifyMutation.mutate(id)
  const handleDelete = (id: number) => deleteMutation.mutate(id)
  const handleOpen = (id: number) => navigate(`/email/domains/${id}`)

  // Provider lookup by id — avoids N+1 fetches per row.
  const providerById = useMemo(() => {
    const map = new Map<number, EmailProvider>()
    providers?.forEach((p) => map.set(p.id, p))
    return map
  }, [providers])

  const hasDomains = domains && domains.length > 0
  const hasProviders = providers && providers.length > 0
  const isLoading = isLoadingDomains || isLoadingProviders

  return (
    <div className="space-y-4">
      {isLoading ? (
        <LoadingSkeleton />
      ) : !hasProviders ? (
        <EmptyState
          icon={Globe}
          title="No email providers configured"
          description="You need to configure an email provider before adding domains. Go to the Providers tab to add one."
        />
      ) : !hasDomains ? (
        <EmptyState
          icon={Globe}
          title="No email domains configured"
          description="Add a domain to start sending emails. You'll need to configure DNS records for verification."
          action={
            <CreateActionButton
              to="/email/domains/new"
              label="Add domain"
            />
          }
        />
      ) : (
        <>
          <div className="mb-4 flex items-center justify-end">
            <CreateActionButton
              to="/email/domains/new"
              label="Add Domain"
            />
          </div>

          <div className="overflow-hidden rounded-lg border">
            <ul role="list" className="divide-y">
              {domains.map((domain) => {
                const provider = providerById.get(domain.provider_id)
                return (
                  <li
                    key={domain.id}
                    onClick={() => handleOpen(domain.id)}
                    className="group flex cursor-pointer items-center gap-4 px-4 py-3 transition-colors hover:bg-muted/40"
                  >
                    {/* Provider logo tile mirrors Storage page's ServiceLogo pattern,
                        with a health dot anchored to the bottom-right. */}
                    <div className="relative shrink-0">
                      <div className="flex size-9 items-center justify-center rounded-md border bg-background">
                        {provider ? (
                          <EmailProviderLogo
                            provider={provider.provider_type as EmailProviderType}
                            size={18}
                          />
                        ) : (
                          <Globe className="size-4 text-muted-foreground" />
                        )}
                      </div>
                      <StatusDot
                        status={domain.status}
                        className="absolute -bottom-0.5 -right-0.5"
                      />
                    </div>

                    <div className="min-w-0 flex-1">
                      <div className="flex items-center gap-2">
                        <p className="truncate text-sm font-medium">
                          {domain.domain}
                        </p>
                        {provider && (
                          <span className="rounded border bg-muted/50 px-1.5 py-0.5 font-mono text-[10px] uppercase tracking-wide text-muted-foreground">
                            {provider.provider_type}
                          </span>
                        )}
                      </div>
                      <div className="mt-1 flex items-center gap-3 text-xs text-muted-foreground">
                        <DomainStatusInline status={domain.status} />
                        <span>
                          {domain.last_verified_at
                            ? `Verified ${formatDistanceToNow(
                                new Date(domain.last_verified_at),
                                { addSuffix: true }
                              )}`
                            : 'Never verified'}
                        </span>
                        <span className="hidden sm:inline">
                          Created {formatDistanceToNow(new Date(domain.created_at), {
                            addSuffix: true,
                          })}
                        </span>
                      </div>
                    </div>

                    <div
                      className="flex items-center gap-1"
                      onClick={(e) => e.stopPropagation()}
                    >
                      <Button
                        variant="ghost"
                        size="icon"
                        className="size-8"
                        onClick={() => handleVerify(domain.id)}
                        disabled={verifyMutation.isPending}
                        title="Verify DNS"
                      >
                        <RefreshCw
                          className={cn(
                            'size-3.5',
                            verifyMutation.isPending &&
                              verifyMutation.variables === domain.id &&
                              'animate-spin'
                          )}
                        />
                        <span className="sr-only">Verify DNS for {domain.domain}</span>
                      </Button>
                      <DropdownMenu>
                        <DropdownMenuTrigger asChild>
                          <Button
                            variant="ghost"
                            size="icon"
                            className="size-8"
                          >
                            <EllipsisVertical className="size-3.5" />
                            <span className="sr-only">Actions for {domain.domain}</span>
                          </Button>
                        </DropdownMenuTrigger>
                        <DropdownMenuContent align="end">
                          <DropdownMenuItem onClick={() => handleOpen(domain.id)}>
                            Open domain
                          </DropdownMenuItem>
                          <DropdownMenuItem onClick={() => handleVerify(domain.id)}>
                            <RefreshCw className="mr-2 size-4" />
                            Verify DNS
                          </DropdownMenuItem>
                          <DropdownMenuSeparator />
                          <DropdownMenuItem
                            className="text-destructive"
                            onClick={() => handleDelete(domain.id)}
                          >
                            Delete
                          </DropdownMenuItem>
                        </DropdownMenuContent>
                      </DropdownMenu>
                    </div>

                    <ChevronRight className="size-4 shrink-0 text-muted-foreground/40 transition-transform group-hover:translate-x-0.5 group-hover:text-muted-foreground" />
                  </li>
                )
              })}
            </ul>
          </div>
        </>
      )}

    </div>
  )
}
