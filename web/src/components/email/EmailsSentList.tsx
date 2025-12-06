'use client'

import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { EmptyState } from '@/components/ui/empty-state'
import { Input } from '@/components/ui/input'
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
import {
  AlertCircle,
  Archive,
  CheckCircle2,
  ChevronLeft,
  ChevronRight,
  Clock,
  Mail,
  MailX,
  Search,
} from 'lucide-react'
import { useState } from 'react'
import { useNavigate } from 'react-router-dom'

// Types
interface Email {
  id: string
  domain_id: number
  project_id: number | null
  from_address: string
  from_name: string | null
  to_addresses: string[]
  cc_addresses: string[] | null
  bcc_addresses: string[] | null
  reply_to: string | null
  subject: string
  html_body: string | null
  text_body: string | null
  headers: Record<string, string> | null
  tags: string[] | null
  status: string
  provider_message_id: string | null
  error_message: string | null
  sent_at: string | null
  created_at: string
}

interface PaginatedEmails {
  data: Email[]
  total: number
  page: number
  page_size: number
}

interface EmailStats {
  total: number
  sent: number
  failed: number
  queued: number
  captured: number
}

interface EmailDomain {
  id: number
  domain: string
}

// API functions
async function listEmails(params: {
  domain_id?: number
  status?: string
  page?: number
  page_size?: number
}): Promise<PaginatedEmails> {
  const searchParams = new URLSearchParams()
  if (params.domain_id) searchParams.set('domain_id', params.domain_id.toString())
  if (params.status) searchParams.set('status', params.status)
  if (params.page) searchParams.set('page', params.page.toString())
  if (params.page_size) searchParams.set('page_size', params.page_size.toString())

  const response = await fetch(`/api/emails?${searchParams}`)
  if (!response.ok) {
    throw new Error('Failed to fetch emails')
  }
  return response.json()
}

async function getEmailStats(domainId?: number): Promise<EmailStats> {
  const url = domainId
    ? `/api/emails/stats?domain_id=${domainId}`
    : '/api/emails/stats'
  const response = await fetch(url)
  if (!response.ok) {
    throw new Error('Failed to fetch email stats')
  }
  return response.json()
}

async function listEmailDomains(): Promise<EmailDomain[]> {
  const response = await fetch('/api/email-domains')
  if (!response.ok) {
    throw new Error('Failed to fetch email domains')
  }
  return response.json()
}

function StatusBadge({ status }: { status: string }) {
  switch (status) {
    case 'sent':
      return (
        <Badge variant="default" className="gap-1">
          <CheckCircle2 className="h-3 w-3" />
          Sent
        </Badge>
      )
    case 'queued':
      return (
        <Badge variant="secondary" className="gap-1">
          <Clock className="h-3 w-3" />
          Queued
        </Badge>
      )
    case 'failed':
      return (
        <Badge variant="destructive" className="gap-1">
          <AlertCircle className="h-3 w-3" />
          Failed
        </Badge>
      )
    case 'captured':
      return (
        <Badge variant="outline" className="gap-1 border-blue-500 text-blue-600">
          <Archive className="h-3 w-3" />
          Captured
        </Badge>
      )
    default:
      return <Badge variant="outline">{status}</Badge>
  }
}

function StatsCard({
  title,
  value,
  icon: Icon,
  description,
}: {
  title: string
  value: number
  icon: React.ElementType
  description?: string
}) {
  return (
    <Card>
      <CardHeader className="flex flex-row items-center justify-between space-y-0 pb-2">
        <CardTitle className="text-sm font-medium">{title}</CardTitle>
        <Icon className="h-4 w-4 text-muted-foreground" />
      </CardHeader>
      <CardContent>
        <div className="text-2xl font-bold">{value.toLocaleString()}</div>
        {description && (
          <p className="text-xs text-muted-foreground">{description}</p>
        )}
      </CardContent>
    </Card>
  )
}

function LoadingSkeleton() {
  return (
    <div className="space-y-4">
      <div className="grid gap-4 md:grid-cols-5">
        {[1, 2, 3, 4, 5].map((i) => (
          <Card key={i}>
            <CardHeader className="pb-2">
              <Skeleton className="h-4 w-16" />
            </CardHeader>
            <CardContent>
              <Skeleton className="h-8 w-12" />
            </CardContent>
          </Card>
        ))}
      </div>
      <Skeleton className="h-10 w-full" />
      <div className="space-y-2">
        {[1, 2, 3, 4, 5].map((i) => (
          <Skeleton key={i} className="h-16 w-full" />
        ))}
      </div>
    </div>
  )
}

export function EmailsSentList() {
  const navigate = useNavigate()
  const [filters, setFilters] = useState({
    domain_id: undefined as number | undefined,
    status: undefined as string | undefined,
    page: 1,
    page_size: 20,
  })

  const { data: stats, isLoading: isLoadingStats } = useQuery({
    queryKey: ['email-stats', filters.domain_id],
    queryFn: () => getEmailStats(filters.domain_id),
  })

  const { data: emails, isLoading: isLoadingEmails } = useQuery({
    queryKey: ['emails', filters],
    queryFn: () => listEmails(filters),
  })

  const { data: domains } = useQuery({
    queryKey: ['email-domains'],
    queryFn: listEmailDomains,
  })

  const totalPages = emails ? Math.ceil(emails.total / filters.page_size) : 0

  const handleFilterChange = (key: string, value: string | number | undefined) => {
    setFilters((prev) => ({
      ...prev,
      [key]: value,
      page: key !== 'page' ? 1 : value as number, // Reset page when filters change
    }))
  }

  if (isLoadingStats && isLoadingEmails) {
    return <LoadingSkeleton />
  }

  const hasEmails = emails && emails.data.length > 0

  return (
    <div className="space-y-6">
      <div>
        <h2 className="text-2xl font-bold tracking-tight">Sent Emails</h2>
        <p className="text-muted-foreground">
          View and track all emails sent through your configured domains.
        </p>
      </div>

      {/* Stats Cards */}
      {stats && (
        <div className="grid gap-4 md:grid-cols-5">
          <StatsCard
            title="Total Emails"
            value={stats.total}
            icon={Mail}
            description="All time"
          />
          <StatsCard
            title="Sent"
            value={stats.sent}
            icon={CheckCircle2}
            description="Successfully delivered"
          />
          <StatsCard
            title="Captured"
            value={stats.captured}
            icon={Archive}
            description="Dev mode (no provider)"
          />
          <StatsCard
            title="Queued"
            value={stats.queued}
            icon={Clock}
            description="Pending delivery"
          />
          <StatsCard
            title="Failed"
            value={stats.failed}
            icon={MailX}
            description="Delivery failed"
          />
        </div>
      )}

      {/* Filters */}
      <div className="flex flex-col sm:flex-row gap-4">
        <div className="flex-1">
          <div className="relative">
            <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4 text-muted-foreground" />
            <Input
              placeholder="Search emails..."
              className="pl-9"
              disabled
            />
          </div>
        </div>
        <Select
          value={filters.domain_id?.toString() ?? 'all'}
          onValueChange={(value) =>
            handleFilterChange('domain_id', value === 'all' ? undefined : parseInt(value))
          }
        >
          <SelectTrigger className="w-full sm:w-[200px]">
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
        <Select
          value={filters.status ?? 'all'}
          onValueChange={(value) =>
            handleFilterChange('status', value === 'all' ? undefined : value)
          }
        >
          <SelectTrigger className="w-full sm:w-[150px]">
            <SelectValue placeholder="All statuses" />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="all">All statuses</SelectItem>
            <SelectItem value="sent">Sent</SelectItem>
            <SelectItem value="captured">Captured</SelectItem>
            <SelectItem value="queued">Queued</SelectItem>
            <SelectItem value="failed">Failed</SelectItem>
          </SelectContent>
        </Select>
      </div>

      {/* Email List */}
      {!hasEmails ? (
        <EmptyState
          icon={Mail}
          title="No emails sent yet"
          description="When you send emails through the API, they will appear here."
        />
      ) : (
        <>
          <div className="rounded-md border">
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Subject</TableHead>
                  <TableHead>To</TableHead>
                  <TableHead>Status</TableHead>
                  <TableHead>Date</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {emails.data.map((email) => (
                  <TableRow
                    key={email.id}
                    className="cursor-pointer hover:bg-muted/50"
                    onClick={() => navigate(`/email/${email.id}`)}
                  >
                    <TableCell className="max-w-[300px]">
                      <div className="font-medium truncate">{email.subject}</div>
                      <div className="text-xs text-muted-foreground truncate">
                        From: {email.from_address}
                      </div>
                    </TableCell>
                    <TableCell className="max-w-[200px]">
                      <div className="truncate">{email.to_addresses.join(', ')}</div>
                    </TableCell>
                    <TableCell>
                      <StatusBadge status={email.status} />
                    </TableCell>
                    <TableCell className="text-muted-foreground">
                      {formatDistanceToNow(
                        new Date(email.sent_at || email.created_at),
                        { addSuffix: true }
                      )}
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </div>

          {/* Pagination */}
          {totalPages > 1 && (
            <div className="flex items-center justify-between">
              <p className="text-sm text-muted-foreground">
                Showing {(filters.page - 1) * filters.page_size + 1} to{' '}
                {Math.min(filters.page * filters.page_size, emails.total)} of{' '}
                {emails.total} emails
              </p>
              <div className="flex items-center gap-2">
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => handleFilterChange('page', filters.page - 1)}
                  disabled={filters.page === 1}
                >
                  <ChevronLeft className="h-4 w-4" />
                  Previous
                </Button>
                <span className="text-sm">
                  Page {filters.page} of {totalPages}
                </span>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => handleFilterChange('page', filters.page + 1)}
                  disabled={filters.page >= totalPages}
                >
                  Next
                  <ChevronRight className="h-4 w-4" />
                </Button>
              </div>
            </div>
          )}
        </>
      )}
    </div>
  )
}
