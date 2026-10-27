// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  approveShieldProposalMutation,
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
import { ReadFailure } from '@/components/ui/read-failure'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { formatLocalDateTime } from '@/lib/date'
import { shieldVerdictBadge } from '@/lib/shield'
import { cn } from '@/lib/utils'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Check, Inbox, X } from 'lucide-react'
import { useState } from 'react'
import { toast } from 'sonner'

type StatusFilter = ShieldProposalStatus | 'all'

const FILTERS: ReadonlyArray<{ value: StatusFilter; label: string }> = [
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

/**
 * Fleet learning's proposals (ADR 066 D9): addresses and fingerprints
 * learning would challenge or block on every service. None applies until an
 * instance admin approves it; a decision can be changed until it expires.
 */
export function ShieldProposalsPanel({
  learning = true,
}: {
  /** Whether fleet learning is on; off, approved proposals do not apply. */
  learning?: boolean
}) {
  const queryClient = useQueryClient()
  const [filter, setFilter] = useState<StatusFilter>('pending')
  const query = filter === 'all' ? {} : { status: filter }
  const proposals = useQuery({ ...listShieldProposalsOptions({ query }) })
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

  const rows = proposals.data ?? []

  return (
    <div className="min-w-0 space-y-3">
      <div className="flex flex-wrap items-start justify-between gap-2">
        <p className="max-w-3xl text-sm text-muted-foreground">
          What Shield learned from attacks across your services. A proposal
          applies only once you approve it, then on every service in selective
          and attack mode until it expires.
          {!learning &&
            ' Learning is off, so approved proposals do not apply until you turn it on.'}
        </p>
        <Select
          value={filter}
          onValueChange={(value) => setFilter(value as StatusFilter)}
        >
          <SelectTrigger
            className="w-36"
            aria-label="Show proposals"
            id="shield-proposals-filter"
          >
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {FILTERS.map((option) => (
              <SelectItem key={option.value} value={option.value}>
                {option.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>
      <div className="min-w-0 rounded-lg border bg-card text-card-foreground">
        {proposals.error && (
          <div className="p-4">
            <ReadFailure
              resource="Shield proposals"
              error={proposals.error}
              cached={proposals.data !== undefined}
              onRetry={() => proposals.refetch()}
              retrying={proposals.isFetching}
              embedded
            />
          </div>
        )}
        {proposals.data === undefined ? (
          proposals.error ? null : (
            <div
              role="status"
              aria-label="Loading Shield proposals"
              className="space-y-2 p-4"
            >
              <Skeleton className="h-10 w-full" />
              <Skeleton className="h-10 w-full" />
            </div>
          )
        ) : rows.length === 0 ? (
          <EmptyState
            size="compact"
            icon={Inbox}
            title={
              filter === 'pending' ? 'No proposals to review' : 'No proposals'
            }
            description={EMPTY_COPY[filter]}
          />
        ) : (
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Subject</TableHead>
                <TableHead>Proposes</TableHead>
                <TableHead className="hidden md:table-cell">Why</TableHead>
                <TableHead className="hidden lg:table-cell">
                  Last seen
                </TableHead>
                <TableHead className="hidden lg:table-cell">Expires</TableHead>
                <TableHead className="text-right">Decision</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {rows.map((proposal) => (
                <TableRow key={proposal.id}>
                  <TableCell>
                    <span
                      className={cn(
                        'font-mono text-xs',
                        proposal.kind === 'ja4'
                          ? 'break-all'
                          : 'whitespace-nowrap'
                      )}
                    >
                      {proposal.subject}
                    </span>
                    <span className="block text-xs text-muted-foreground">
                      {proposal.kind === 'ja4' ? 'JA4 fingerprint' : 'Address'}
                    </span>
                  </TableCell>
                  <TableCell>
                    <Badge variant={shieldVerdictBadge(proposal.verdict)}>
                      {proposal.verdict}
                    </Badge>
                  </TableCell>
                  <TableCell className="hidden max-w-[22rem] text-sm text-muted-foreground md:table-cell">
                    {proposal.reason}
                    <span className="block text-xs">
                      {proposal.hits.toLocaleString()}{' '}
                      {proposal.hits === 1 ? 'event' : 'events'}
                    </span>
                  </TableCell>
                  <TableCell className="hidden text-sm text-muted-foreground lg:table-cell">
                    {formatLocalDateTime(proposal.last_seen)}
                  </TableCell>
                  <TableCell className="hidden text-sm text-muted-foreground lg:table-cell">
                    {formatLocalDateTime(proposal.expires_at)}
                  </TableCell>
                  <TableCell className="text-right">
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
                            approve.mutate({
                              path: { proposal_id: proposal.id },
                            })
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
                            reject.mutate({
                              path: { proposal_id: proposal.id },
                            })
                          }
                        >
                          <X className="h-4 w-4" />
                          <span className="ml-1 max-sm:sr-only">Reject</span>
                        </Button>
                      )}
                    </div>
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        )}
      </div>
    </div>
  )
}
