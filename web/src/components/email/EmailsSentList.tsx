// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

'use client'

import { listEmails, type PaginatedEmailsResponse } from '@/api/client'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { ResponsivePagination } from '@/components/ui/responsive-pagination'
import { EmptyState } from '@/components/ui/empty-state'
import { RecordLink, RecordRow } from '@temps-sdk/ds'
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
import { useQuery } from '@tanstack/react-query'
import { formatDistanceToNow } from 'date-fns'
import { AlertCircle, Eye, Mail, MousePointerClick } from 'lucide-react'
import { useState } from 'react'
import { Link } from 'react-router'
import { fetchEmailDomains } from './email-queries'
import { StatusBadge } from './shared'
import { problemMessage } from './sharedUtils'

const PAGE_SIZE = 10

async function fetchEmails(params: {
  domain_id?: number
  status?: string
  page: number
  page_size: number
}): Promise<PaginatedEmailsResponse> {
  const response = await listEmails({ query: params })
  if (response.error || !response.data) {
    throw new Error(problemMessage(response.error, 'Failed to fetch emails'))
  }
  return response.data
}

function LoadingSkeleton() {
  return (
    <div className="space-y-2">
      {[1, 2, 3].map((i) => (
        <Skeleton key={i} className="h-14 w-full" />
      ))}
    </div>
  )
}

/**
 * Emails sent through this install. Given a `domainId` it lists that domain's
 * only, so the domain's own page can show them without a filter to set.
 */
export function EmailsSentList({ domainId }: { domainId?: number }) {
  const [domainFilter, setDomainFilter] = useState<number>()
  const [status, setStatus] = useState<string>()
  const [page, setPage] = useState(1)
  const scoped = domainId !== undefined
  const filters = {
    domain_id: scoped ? domainId : domainFilter,
    status,
    page,
    page_size: PAGE_SIZE,
  }

  const emailsQuery = useQuery({
    queryKey: ['emails', filters],
    queryFn: () => fetchEmails(filters),
  })
  const emails = emailsQuery.data

  const { data: domains } = useQuery({
    queryKey: ['email-domains'],
    queryFn: fetchEmailDomains,
    enabled: !scoped,
  })

  const totalPages = emails ? Math.ceil(emails.total / PAGE_SIZE) : 0
  const filtered = status !== undefined || domainFilter !== undefined

  return (
    <div className="@container space-y-4">
      <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        <h2 className="text-base font-semibold">Sent emails</h2>
        <div className="flex flex-col gap-2 sm:flex-row sm:flex-wrap">
          {!scoped && (
            <Select
              value={domainFilter?.toString() ?? 'all'}
              onValueChange={(value) => {
                setDomainFilter(value === 'all' ? undefined : parseInt(value))
                setPage(1)
              }}
            >
              <SelectTrigger
                aria-label="Filter by domain"
                className="w-full sm:w-[200px]"
              >
                <SelectValue placeholder="All domains" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all">All domains</SelectItem>
                {domains?.map((domain) => (
                  <SelectItem key={domain.id} value={domain.id.toString()}>
                    {domain.domain}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          )}
          <Select
            value={status ?? 'all'}
            onValueChange={(value) => {
              setStatus(value === 'all' ? undefined : value)
              setPage(1)
            }}
          >
            <SelectTrigger
              aria-label="Filter by status"
              className="w-full sm:w-[160px]"
            >
              <SelectValue placeholder="All statuses" />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="all">All statuses</SelectItem>
              <SelectItem value="sent">Sent</SelectItem>
              <SelectItem value="captured">Captured</SelectItem>
              <SelectItem value="queued">Queued</SelectItem>
              <SelectItem value="sending">Sending</SelectItem>
              <SelectItem value="delivery_unknown">Delivery unknown</SelectItem>
              <SelectItem value="failed">Failed</SelectItem>
            </SelectContent>
          </Select>
        </div>
      </div>

      {emailsQuery.isLoading ? (
        <LoadingSkeleton />
      ) : emailsQuery.isError ? (
        <Alert role="alert">
          <AlertCircle className="size-4" />
          <AlertTitle>Couldn&apos;t load sent emails</AlertTitle>
          <AlertDescription className="space-y-3">
            <p>{emailsQuery.error.message}</p>
            <Button
              size="sm"
              variant="outline"
              disabled={emailsQuery.isFetching}
              onClick={() => void emailsQuery.refetch()}
            >
              Retry
            </Button>
          </AlertDescription>
        </Alert>
      ) : !emails || emails.data.length === 0 ? (
        filtered ? (
          <EmptyState
            size="compact"
            icon={Mail}
            title="No emails match these filters"
            description="Try another domain or status."
            action={
              <Button
                variant="outline"
                size="sm"
                onClick={() => {
                  setDomainFilter(undefined)
                  setStatus(undefined)
                  setPage(1)
                }}
              >
                Clear filters
              </Button>
            }
          />
        ) : (
          <EmptyState
            size="compact"
            icon={Mail}
            title={
              scoped
                ? 'Nothing sent from this domain yet'
                : 'No emails sent yet'
            }
            description="Emails sent through the API or SDK show up here, with what happened to each one."
            action={
              <Button variant="outline" size="sm" asChild>
                <Link to="/email?tab=sdk">See how to send</Link>
              </Button>
            }
          />
        )
      ) : (
        <>
          <div className="rounded-md border">
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Subject</TableHead>
                  <TableHead className="hidden @xl:table-cell">To</TableHead>
                  <TableHead>Status</TableHead>
                  <TableHead className="hidden @3xl:table-cell">
                    Opens
                  </TableHead>
                  <TableHead className="hidden @3xl:table-cell">
                    Clicks
                  </TableHead>
                  <TableHead className="hidden @lg:table-cell">Date</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {emails.data.map((email) => (
                  <RecordRow key={email.id}>
                    <TableCell className="max-w-[300px]">
                      <RecordLink to={`/email/${email.id}`} className="min-h-0">
                        {email.subject}
                      </RecordLink>
                      <div className="truncate text-xs text-muted-foreground">
                        From: {email.from_address}
                      </div>
                    </TableCell>
                    <TableCell className="hidden max-w-[200px] @xl:table-cell">
                      <div className="truncate">
                        {email.to_addresses.join(', ')}
                      </div>
                    </TableCell>
                    <TableCell>
                      <StatusBadge status={email.status} />
                    </TableCell>
                    <TableCell className="hidden @3xl:table-cell">
                      {email.track_opens ? (
                        <span className="flex items-center gap-1 text-sm">
                          <Eye className="h-3 w-3 text-muted-foreground" />
                          {email.open_count}
                        </span>
                      ) : (
                        <span className="text-xs text-muted-foreground">
                          --
                        </span>
                      )}
                    </TableCell>
                    <TableCell className="hidden @3xl:table-cell">
                      {email.track_clicks ? (
                        <span className="flex items-center gap-1 text-sm">
                          <MousePointerClick className="h-3 w-3 text-muted-foreground" />
                          {email.click_count}
                        </span>
                      ) : (
                        <span className="text-xs text-muted-foreground">
                          --
                        </span>
                      )}
                    </TableCell>
                    <TableCell className="hidden whitespace-nowrap text-muted-foreground @lg:table-cell">
                      {formatDistanceToNow(
                        new Date(email.sent_at || email.created_at),
                        { addSuffix: true }
                      )}
                    </TableCell>
                  </RecordRow>
                ))}
              </TableBody>
            </Table>
          </div>

          {totalPages > 1 && (
            <ResponsivePagination
              page={page}
              pageSize={PAGE_SIZE}
              total={emails.total}
              totalPages={totalPages}
              onPageChange={setPage}
              ariaLabel="Sent emails pagination"
            />
          )}
        </>
      )}
    </div>
  )
}
