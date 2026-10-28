// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  approveShieldProposalMutation,
  getShieldSettingsOptions,
  listShieldProposalsOptions,
  listShieldProposalsQueryKey,
  rejectShieldProposalMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  ShieldProposalResponse,
  ShieldProposalStatus,
} from '@/api/client/types.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { EmptyState } from '@/components/ui/empty-state'
import { Input } from '@/components/ui/input'
import { ReadFailure } from '@/components/ui/read-failure'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { shieldVerdictBadge } from '@/lib/shield'
import { cn } from '@/lib/utils'
import {
  Callout,
  DataTable,
  fmtNumber,
  fmtRelativeTime,
  PageHeader,
  useUrlSearchText,
  useUrlState,
  type DataTableColumn,
} from '@temps-sdk/ds'
import {
  keepPreviousData,
  useMutation,
  useQuery,
  useQueryClient,
} from '@tanstack/react-query'
import { Check, Inbox, Search, X } from 'lucide-react'
import { useEffect } from 'react'
import { Link } from 'react-router'
import { toast } from 'sonner'

type UrlKey = 'q' | 'status' | 'page' | 'per_page'

type StatusFilter = ShieldProposalStatus | 'all'

const STATUSES: ReadonlyArray<{ value: StatusFilter; label: string }> = [
  { value: 'pending', label: 'Pending' },
  { value: 'approved', label: 'Approved' },
  { value: 'rejected', label: 'Rejected' },
  { value: 'all', label: 'All' },
]

const STATUS_BADGE = {
  pending: 'warning-subtle',
  approved: 'success-subtle',
  rejected: 'neutral-subtle',
} as const

const EMPTY_COPY: Record<StatusFilter, string> = {
  pending:
    'Nothing waits for review. When an address sends exploit probes or keeps failing challenges, Shield proposes acting on it here.',
  approved: 'No approved proposal applies right now.',
  rejected: 'No rejected proposal is kept right now.',
  all: 'Shield has proposed nothing that is still current.',
}

const PAGE_SIZES = [10, 25, 50, 100] as const
const DEFAULT_PAGE_SIZE = 25

/** Where the learning switch lives. */
const SHIELD_SETTINGS = '/settings/security#shield'

function positive(value: string | undefined, fallback: number) {
  const parsed = Number(value)
  return Number.isInteger(parsed) && parsed > 0 ? parsed : fallback
}

/**
 * Fleet learning's proposals (ADR 066 D9): addresses and fingerprints
 * learning would challenge or block on every service. None applies until an
 * instance admin approves it; a decision can be changed until it expires.
 * Route: /settings/shield-proposals.
 */
export function ShieldProposalsPage() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const queryClient = useQueryClient()
  const url = useUrlState<UrlKey>()
  const [searchText, setSearchText, search] = useUrlSearchText('q', {
    resetKeys: ['page'],
    urlState: url,
  })
  const status: StatusFilter =
    STATUSES.find((option) => option.value === url.state.status)?.value ??
    'pending'
  const requestedSize = positive(url.state.per_page, DEFAULT_PAGE_SIZE)
  const pageSize = (PAGE_SIZES as readonly number[]).includes(requestedSize)
    ? requestedSize
    : DEFAULT_PAGE_SIZE
  const page = positive(url.state.page, 1)
  const q = search.trim()

  usePageTitle('Shield proposals')
  useEffect(() => {
    setBreadcrumbs([
      { label: 'Settings', href: '/settings' },
      { label: 'Shield proposals' },
    ])
  }, [setBreadcrumbs])

  const settings = useQuery({ ...getShieldSettingsOptions() })
  const learning = settings.data?.share_observations ?? true
  const proposals = useQuery({
    ...listShieldProposalsOptions({
      query: {
        ...(status === 'all' ? {} : { status }),
        ...(q === '' ? {} : { q }),
        page,
        page_size: pageSize,
      },
    }),
    // The rows stay while the next page loads, so the table does not jump.
    placeholderData: keepPreviousData,
  })
  const total = proposals.data?.total ?? 0
  const totalPages = Math.max(1, Math.ceil(total / pageSize))
  // Deciding the last row of the last page, or a stale `?page=`, points
  // past the end: go to the new last page.
  const pastEnd =
    proposals.data !== undefined &&
    !proposals.isPlaceholderData &&
    page > totalPages
  useEffect(() => {
    if (pastEnd) url.patch({ page: totalPages === 1 ? undefined : totalPages })
  }, [pastEnd, totalPages, url])

  const invalidate = () =>
    queryClient.invalidateQueries({ queryKey: listShieldProposalsQueryKey() })
  const approve = useMutation({
    ...approveShieldProposalMutation(),
    meta: { errorTitle: 'Failed to approve the proposal' },
    onSuccess: (saved) => {
      toast.success(
        `${saved.subject} is ${saved.verdict === 'block' ? 'blocked' : 'challenged'} on every service in selective and attack mode`
      )
      void invalidate()
    },
  })
  const reject = useMutation({
    ...rejectShieldProposalMutation(),
    meta: { errorTitle: 'Failed to reject the proposal' },
    onSuccess: (saved) => {
      toast.success(`Rejected the proposal for ${saved.subject}`)
      void invalidate()
    },
  })
  const busy = (proposal: ShieldProposalResponse) =>
    (approve.isPending &&
      approve.variables?.path.proposal_id === proposal.id) ||
    (reject.isPending && reject.variables?.path.proposal_id === proposal.id)

  const columns: DataTableColumn<ShieldProposalResponse>[] = [
    {
      key: 'subject',
      header: 'Subject',
      className: 'max-w-xs',
      render: (proposal) => (
        <>
          <span
            className={cn(
              'font-mono text-xs',
              proposal.kind === 'ja4' ? 'break-all' : 'whitespace-nowrap'
            )}
          >
            {proposal.subject}
          </span>
          <span className="block text-xs text-muted-foreground">
            {proposal.kind === 'ja4' ? 'JA4 fingerprint' : 'Address'}
          </span>
        </>
      ),
    },
    {
      key: 'verdict',
      header: 'Proposes',
      render: (proposal) => (
        <Badge variant={shieldVerdictBadge(proposal.verdict)}>
          {proposal.verdict}
        </Badge>
      ),
    },
    {
      key: 'why',
      header: 'Why',
      className: 'max-w-sm text-muted-foreground',
      render: (proposal) => (
        <>
          {proposal.reason}
          <span className="block text-xs">
            {fmtNumber(proposal.hits)}{' '}
            {proposal.hits === 1 ? 'event' : 'events'}
          </span>
        </>
      ),
    },
    {
      key: 'last_seen',
      header: 'Last seen',
      className: 'whitespace-nowrap text-muted-foreground',
      ariaSort: 'descending',
      render: (proposal) => fmtRelativeTime(proposal.last_seen),
    },
    {
      key: 'expires_at',
      header: 'Expires',
      className: 'whitespace-nowrap text-muted-foreground',
      render: (proposal) => fmtRelativeTime(proposal.expires_at),
    },
    {
      key: 'decision',
      header: 'Decision',
      className: 'text-right',
      render: (proposal) => (
        <div className="flex flex-wrap items-center justify-end gap-2">
          {proposal.status !== 'pending' && (
            <Badge variant={STATUS_BADGE[proposal.status]}>
              {proposal.status}
            </Badge>
          )}
          {proposal.status !== 'approved' && (
            <Button
              type="button"
              size="sm"
              disabled={busy(proposal)}
              aria-label={`Approve ${proposal.subject}`}
              onClick={() =>
                approve.mutate({ path: { proposal_id: proposal.id } })
              }
            >
              <Check className="h-4 w-4" />
              <span className="ml-1 max-sm:sr-only">Approve</span>
            </Button>
          )}
          {proposal.status !== 'rejected' && (
            <Button
              type="button"
              size="sm"
              variant="outline"
              disabled={busy(proposal)}
              aria-label={`Reject ${proposal.subject}`}
              onClick={() =>
                reject.mutate({ path: { proposal_id: proposal.id } })
              }
            >
              <X className="h-4 w-4" />
              <span className="ml-1 max-sm:sr-only">Reject</span>
            </Button>
          )}
        </div>
      ),
    },
  ]

  const isFiltered = q !== ''

  const isLoading = proposals.data === undefined && !proposals.error
  const rows = proposals.data?.items ?? []

  // The settings layout is the page container, so this renders the header,
  // toolbar and table inside it rather than a second shell (`Ledger`).
  return (
    <div className="space-y-6">
      <PageHeader
        title="Shield proposals"
        description="What Shield learned from attacks across your services. A proposal applies only once you approve it, then on every service in selective and attack mode until it expires."
      />
      <div className="space-y-4">
        {!learning && (
          <Callout tone="warning" title="Learning is off">
            Approved proposals do not apply, and Shield proposes nothing new,
            until you turn on{' '}
            <Link to={SHIELD_SETTINGS} className="underline">
              Learn from attacks across all services
            </Link>
            .
          </Callout>
        )}
        <div className="flex flex-col gap-2 sm:flex-row sm:flex-wrap sm:items-center">
          <div className="relative w-full sm:w-64">
            <Search
              aria-hidden
              className="pointer-events-none absolute left-2.5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground"
            />
            <Input
              type="search"
              value={searchText}
              onChange={(event) => setSearchText(event.target.value)}
              placeholder="Address or fingerprint"
              aria-label="Search proposals"
              className="pl-8"
              maxLength={64}
            />
          </div>
          <Select
            value={status}
            onValueChange={(value) =>
              url.patch({
                status: value === 'pending' ? undefined : value,
                page: undefined,
              })
            }
          >
            <SelectTrigger
              className="w-full sm:w-40"
              aria-label="Show proposals"
            >
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {STATUSES.map((option) => (
                <SelectItem key={option.value} value={option.value}>
                  {option.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
        {proposals.error ? (
          <ReadFailure
            resource="Shield proposals"
            error={proposals.error}
            cached={proposals.data !== undefined}
            onRetry={() => proposals.refetch()}
            retrying={proposals.isFetching}
          />
        ) : null}
      </div>
      {!isLoading && rows.length === 0 ? (
        proposals.error ? null : (
          <EmptyState
            size="compact"
            icon={Inbox}
            title={
              isFiltered
                ? 'No proposals match'
                : status === 'pending'
                  ? 'No proposals to review'
                  : 'No proposals'
            }
            description={
              isFiltered
                ? `No proposal for an address or fingerprint containing “${q}”.`
                : EMPTY_COPY[status]
            }
            action={
              isFiltered ? (
                <Button variant="outline" onClick={() => setSearchText('')}>
                  Clear search
                </Button>
              ) : undefined
            }
          />
        )
      ) : (
        <DataTable
          columns={columns}
          rows={rows}
          rowKey={(proposal) => proposal.id}
          isLoading={isLoading}
          pagination={
            total > 0
              ? {
                  page: Math.min(page, totalPages),
                  pageSize,
                  total,
                  totalPages,
                  pageSizeOptions: PAGE_SIZES,
                  ariaLabel: 'Proposals pagination',
                  pageSizeAriaLabel: 'Proposals per page',
                  onPageChange: (next) =>
                    url.patch({ page: next === 1 ? undefined : next }),
                  onPageSizeChange: (next) =>
                    url.patch({
                      per_page: next === DEFAULT_PAGE_SIZE ? undefined : next,
                      page: undefined,
                    }),
                }
              : undefined
          }
        />
      )}
    </div>
  )
}
