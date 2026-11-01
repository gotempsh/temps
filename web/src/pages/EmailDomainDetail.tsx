// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

'use client'

import {
  deleteEmailDomain,
  getDomain,
  getEmailStats,
  listEmailDomainProjects,
  authorizeEmailDomainProject,
  revokeEmailDomainProject,
  listDnsProviders,
  listEmailDomains,
  listEmailProviders,
  setupDns,
  verifyDomain,
  type DnsProviderResponse,
  type EmailDomainResponse,
  type EmailDomainWithDnsResponse,
  type EmailProviderResponse,
  type EmailStatsResponse,
  type AuthorizedEmailDomainProjectResponse,
  type SetupDnsResponse,
} from '@/api/client'
import {
  DnsRecordsTable,
  DnsVerificationSummary,
} from '@/components/email/EmailDomainsManagement'
import { isAdvisoryDnsRecord } from '@/lib/email-dns-records'
import { isSmtpProvider, smtpVerifyToast } from '@/lib/email-smtp-domains'
import { getProjectsOptions } from '@/api/client/@tanstack/react-query.gen'
import { ProjectSelect } from '@/components/project/ProjectSelect'
import { Badge } from '@/components/ui/badge'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
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
import {
  EmailProviderLogo,
  type EmailProviderType,
} from '@/components/ui/email-provider-logo'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Separator } from '@/components/ui/separator'
import { Skeleton } from '@/components/ui/skeleton'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { useAuth } from '@/contexts/AuthContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { errorWithStatus } from '@/lib/http-error-status'
import { isVerifiedNotFound } from '@/lib/read-failure'
import { cn } from '@/lib/utils'
import {
  Button,
  Callout,
  Detail,
  PageState,
  Status,
  fmtNumber,
  fmtRelativeTime,
  fmtDateTime,
  type DetailFact,
  type StatusTone,
} from '@temps-sdk/ds'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  AlertCircle,
  ArrowLeft,
  CheckCircle2,
  Globe,
  KeyRound,
  Loader2,
  Plus,
  RefreshCw,
  Settings2,
  Trash2,
  Wand2,
} from 'lucide-react'
import { PublicIngressNotice } from '@/components/settings/PublicIngressNotice'
import { useEffect, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router'
import { toast } from 'sonner'
import { problemMessage } from '@/components/email/sharedUtils'

async function fetchDomain(id: number): Promise<EmailDomainWithDnsResponse> {
  const response = await getDomain({ path: { id } })
  if (response.error || !response.data) {
    // Keep the HTTP status on the error: a 404 is an answer the page shows
    // at once, not a failure the retry policy repeats three times.
    throw errorWithStatus(
      problemMessage(response.error, 'Failed to fetch email domain'),
      response.error
    )
  }
  return response.data
}

async function fetchProviders(): Promise<EmailProviderResponse[]> {
  const response = await listEmailProviders()
  if (response.error) {
    throw errorWithStatus(problemMessage(response.error, 'Failed to fetch email providers'), response.error)
  }
  return response.data ?? []
}

async function fetchStoredDomains(): Promise<EmailDomainResponse[]> {
  const response = await listEmailDomains()
  if (response.error) {
    throw errorWithStatus(problemMessage(response.error, 'Failed to fetch email domains'), response.error)
  }
  return response.data ?? []
}

async function fetchDnsProviders(): Promise<DnsProviderResponse[]> {
  const response = await listDnsProviders()
  if (response.error) {
    throw errorWithStatus(problemMessage(response.error, 'Failed to fetch DNS providers'), response.error)
  }
  return response.data ?? []
}

// NOTE: EmailAnalytics.tsx's bounce/complaint/open/click *rate* stats come
// from `/emails/events/stats` (get_global_event_stats), which has no
// domain_id filter at all (global-only). `getEmailStats` (/emails/stats,
// used by EmailsSentList.tsx) does support a domain_id filter, so that's
// what we use here for domain-scoped delivery stats.
async function fetchEmailStats(domainId: number): Promise<EmailStatsResponse> {
  const response = await getEmailStats({ query: { domain_id: domainId } })
  if (response.error || !response.data) {
    throw errorWithStatus(problemMessage(response.error, 'Failed to fetch email stats'), response.error)
  }
  return response.data
}

async function fetchAuthorizedProjects(domainId: number): Promise<AuthorizedEmailDomainProjectResponse[]> {
  const response = await listEmailDomainProjects({ path: { id: domainId } })
  if (response.error) {
    throw errorWithStatus(problemMessage(response.error, 'Failed to fetch authorized services'), response.error)
  }
  return response.data ?? []
}

// Mirrors `StatusPill` (components/email/EmailDomainsManagement.tsx)
// tone-for-tone rather than inventing a new severity ordering — this is the
// Detail template's single verdict, derived straight from the record's own
// status field.
const DOMAIN_STATUS_VERDICT: Record<string, { tone: StatusTone; label: string }> = {
  verified: { tone: 'ok', label: 'Verified' },
  pending: { tone: 'warn', label: 'Pending' },
  failed: { tone: 'error', label: 'Failed' },
}

function domainVerdict(
  status: string | undefined,
  isSmtp: boolean
): { tone: StatusTone; label: string } {
  if (status === undefined) {
    return { tone: 'idle', label: 'Status unknown' }
  }
  // Temps verifies nothing for SMTP domains; the stored status only says
  // whether the domain may send yet.
  if (isSmtp) {
    return status === 'verified'
      ? { tone: 'ok', label: 'Sending enabled' }
      : { tone: 'warn', label: 'Sending paused' }
  }
  return DOMAIN_STATUS_VERDICT[status] ?? { tone: 'idle', label: status }
}

function domainFacts(
  domain: EmailDomainResponse,
  provider: EmailProviderResponse | undefined,
  verifiedCount: number,
  totalCount: number,
  isSmtp: boolean
): DetailFact[] {
  const added: DetailFact = {
    label: 'Added',
    value: (
      <span title={fmtDateTime(domain.created_at)}>{fmtRelativeTime(domain.created_at)}</span>
    ),
  }
  if (isSmtp) {
    return [
      {
        label: 'Provider',
        value: provider ? (
          <span className="inline-flex items-center gap-1.5">
            <EmailProviderLogo provider={provider.provider_type as EmailProviderType} size={14} />
            {provider.name}
          </span>
        ) : (
          '—'
        ),
      },
      { label: 'DNS', value: 'Managed upstream' },
      added,
    ]
  }
  return [
    {
      label: 'Provider',
      value: provider ? (
        <span className="inline-flex items-center gap-1.5">
          <EmailProviderLogo provider={provider.provider_type as EmailProviderType} size={14} />
          {provider.name}
        </span>
      ) : (
        '—'
      ),
    },
    {
      label: 'Records',
      value: totalCount > 0 ? `${verifiedCount} / ${totalCount} verified` : '—',
    },
    {
      label: 'Last verified',
      value: domain.last_verified_at ? (
        <span title={fmtDateTime(domain.last_verified_at)}>
          {fmtRelativeTime(domain.last_verified_at)}
        </span>
      ) : (
        'Never'
      ),
    },
    added,
  ]
}

const STAT_DIVIDER_CLASSES = cn(
  // Mobile: 2 columns — vertical divider on the right column, horizontal
  // divider once a second row starts (items 3+, since there are 5 items).
  '[&:nth-child(2n)]:border-l',
  '[&:nth-child(n+3)]:border-t',
  // Desktop: single row — only a vertical divider between siblings,
  // no leftover horizontal divider from the mobile layout.
  '@min-3xl:border-t-0',
  '@min-3xl:[&:not(:first-child)]:border-l',
  '@min-3xl:[&:nth-child(2n)]:border-l'
)

function StatPanel({ stats }: { stats: { label: string; value: number }[] }) {
  return (
    <div className="@container rounded-lg border">
      <dl className="grid grid-cols-2 @min-3xl:grid-cols-7">
        {stats.map((stat) => (
          <div key={stat.label} className={cn('space-y-1 p-4', STAT_DIVIDER_CLASSES)}>
            <dt className="truncate text-sm font-medium text-muted-foreground">
              {stat.label}
            </dt>
            <dd className="text-2xl font-semibold tabular-nums">
              {fmtNumber(stat.value)}
            </dd>
          </div>
        ))}
      </dl>
    </div>
  )
}

function StatsSkeleton() {
  return (
    <div className="@container rounded-lg border">
      <dl className="grid grid-cols-2 @min-3xl:grid-cols-7">
        {[1, 2, 3, 4, 5, 6, 7].map((i) => (
          <div key={i} className={cn('space-y-2 p-4', STAT_DIVIDER_CLASSES)}>
            <Skeleton className="h-4 w-16" />
            <Skeleton className="h-7 w-12" />
          </div>
        ))}
      </dl>
    </div>
  )
}

function EmailDomainDetailSkeleton({ backAction }: { backAction: React.ReactNode }) {
  return (
    <Detail
      title={<Skeleton className="h-7 w-56" />}
      actions={backAction}
      facts={[0, 1, 2, 3].map(() => ({
        label: <Skeleton className="h-3 w-16" />,
        value: <Skeleton className="h-4 w-24" />,
      }))}
      main={
        <>
          <StatsSkeleton />
          <Card>
            <CardHeader>
              <Skeleton className="h-5 w-32" />
              <Skeleton className="mt-2 h-4 w-80" />
            </CardHeader>
            <CardContent className="space-y-4">
              <Skeleton className="h-14 w-full rounded-lg" />
              <div className="rounded-md border">
                <div className="space-y-3 p-4">
                  <Skeleton className="h-4 w-full" />
                  <Skeleton className="h-4 w-11/12" />
                  <Skeleton className="h-4 w-10/12" />
                  <Skeleton className="h-4 w-9/12" />
                </div>
              </div>
            </CardContent>
          </Card>
        </>
      }
      aside={<Skeleton className="h-48 w-full rounded-lg" />}
    />
  )
}

export function EmailDomainDetail() {
  const { id: idParam } = useParams<{ id: string }>()
  const id = idParam ? parseInt(idParam, 10) : undefined
  const { setBreadcrumbs } = useBreadcrumbs()
  const queryClient = useQueryClient()
  const navigate = useNavigate()
  const { user } = useAuth()
  const canManageAuthorizations = user?.role === 'admin' || user?.role === 'platform_admin'

  const [selectedDnsProviderId, setSelectedDnsProviderId] = useState<number | null>(null)
  const [dnsSetupResult, setDnsSetupResult] = useState<SetupDnsResponse | null>(null)
  const [projectToRevoke, setProjectToRevoke] = useState<AuthorizedEmailDomainProjectResponse | null>(null)
  const [confirmingDelete, setConfirmingDelete] = useState(false)
  const [projectToAuthorize, setProjectToAuthorize] = useState<number | null>(null)
  const [projectPickerOpen, setProjectPickerOpen] = useState(false)

  const {
    data: domainDetails,
    isLoading,
    error: fetchError,
    refetch: refetchDomain,
  } = useQuery({
    queryKey: ['email-domain', id],
    queryFn: () => fetchDomain(id!),
    enabled: !!id,
  })

  const {
    data: emailStats,
    isLoading: isLoadingStats,
    error: statsError,
    refetch: refetchStats,
  } = useQuery({
    queryKey: ['email-stats', id],
    queryFn: () => fetchEmailStats(id!),
    enabled: !!id,
  })

  const { data: providers, isLoading: isLoadingProviders } = useQuery({
    queryKey: ['email-providers'],
    queryFn: fetchProviders,
  })

  // The detail endpoint reports the provider's live status, which for SMTP is
  // always "verified" since Temps checks no DNS for it. Sending is gated on
  // the *stored* status (still "pending" after creation or a credentials
  // change), which only the list endpoint returns. Never fall back to the
  // live status for SMTP: that would report a paused domain as sending.
  const {
    data: storedDomains,
    isLoading: isLoadingStoredDomains,
    refetch: refetchStoredDomains,
  } = useQuery({
    queryKey: ['email-domains'],
    queryFn: fetchStoredDomains,
  })

  const { data: dnsProviders } = useQuery({
    queryKey: ['dns-providers'],
    queryFn: fetchDnsProviders,
  })

  const {
    data: authorizedProjects = [],
    isLoading: isLoadingAuthorizations,
    error: authorizationsError,
    refetch: refetchAuthorizations,
  } = useQuery({
    queryKey: ['email-domain-projects', id],
    queryFn: () => fetchAuthorizedProjects(id!),
    enabled: !!id,
  })

  const domain = domainDetails?.domain
  const dnsRecords = domainDetails?.dns_records ?? []
  const provider = providers?.find((p) => p.id === domain?.provider_id)
  const isSmtp = isSmtpProvider(provider)

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Email', href: '/email' },
      { label: 'Domains', href: '/email?tab=domains' },
      { label: domain?.domain ?? 'Domain' },
    ])
  }, [setBreadcrumbs, domain?.domain])

  usePageTitle(domain?.domain ?? 'Email Domain')

  const verifyMutation = useMutation({
    mutationFn: async () => {
      const response = await verifyDomain({ path: { id: id! } })
      if (response.error || !response.data) {
        throw new Error(problemMessage(response.error, 'Failed to verify email domain'))
      }
      return response.data
    },
    onSuccess: (data) => {
      if (isSmtpProvider(provider)) {
        smtpVerifyToast(data, provider)
        queryClient.setQueryData(['email-domain', data.domain.id], data)
        queryClient.invalidateQueries({ queryKey: ['email-domains'] })
        return
      }
      // Advisory records (MX, DMARC) are excluded from the backend's
      // verification gate — exclude them from the counts shown here too, so
      // the toast reflects the records that actually gate the status.
      const required = data.dns_records.filter(r => !isAdvisoryDnsRecord(r))
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
      queryClient.invalidateQueries({ queryKey: ['email-domains'] })
    },
    onError: (err: Error) => {
      toast.error('Failed to verify domain', { description: err.message })
    },
  })

  const deleteMutation = useMutation({
    mutationFn: async () => {
      const response = await deleteEmailDomain({ path: { id: id! } })
      if (response.error) {
        throw new Error(problemMessage(response.error, 'Failed to delete email domain'))
      }
    },
    onSuccess: () => {
      toast.success('Domain deleted')
      queryClient.invalidateQueries({ queryKey: ['email-domains'] })
      navigate('/email?tab=domains')
    },
    onError: (err: Error) => {
      toast.error('Failed to delete domain', { description: err.message })
    },
  })

  const setupDnsMutation = useMutation({
    mutationFn: async ({ dnsProviderId }: { dnsProviderId: number }) => {
      const response = await setupDns({
        path: { id: id! },
        body: { dns_provider_id: dnsProviderId },
      })
      if (response.error || !response.data) {
        throw new Error(problemMessage(response.error, 'Failed to setup DNS records'))
      }
      return response.data
    },
    onSuccess: (data) => {
      setDnsSetupResult(data)
      if (data.success) {
        toast.success('DNS records created', {
          description: `${data.records_created} of ${data.total_records} records were created automatically.`,
        })
        verifyMutation.mutate()
      } else if (data.records_created > 0) {
        toast.warning('Some DNS records created', {
          description: `${data.records_created} of ${data.total_records} records were created.`,
        })
      } else {
        toast.error('Failed to create DNS records', { description: data.message })
      }
    },
    onError: (err: Error) => {
      toast.error('Failed to setup DNS records', { description: err.message })
    },
  })

  // Shares its cache with ProjectSelect's internal query (same key/args), so
  // this adds no extra request — it only exists to resolve a name for the
  // confirmation dialog below.
  const projectsQuery = useQuery({
    ...getProjectsOptions({ query: { page: 1, per_page: 100 } }),
    staleTime: 60_000,
  })
  const hasNoProjects = projectsQuery.data?.total === 0
  const projectToAuthorizeName = projectsQuery.data?.projects.find(
    (p) => p.id === projectToAuthorize
  )?.name

  const authorizeProjectMutation = useMutation({
    mutationFn: async (projectId: number) => {
      const response = await authorizeEmailDomainProject({ path: { id: id!, project_id: projectId } })
      if (response.error) {
        throw new Error(problemMessage(response.error, 'Failed to authorize service'))
      }
    },
    onSuccess: () => {
      setProjectToAuthorize(null)
      queryClient.invalidateQueries({ queryKey: ['email-domain-projects', id] })
      toast.success('Service authorized', {
        description: 'Deployments in this service can now send from this domain.',
      })
    },
    onError: (error: Error) => toast.error('Failed to authorize service', { description: error.message }),
  })

  const revokeProjectMutation = useMutation({
    mutationFn: async (projectId: number) => {
      const response = await revokeEmailDomainProject({ path: { id: id!, project_id: projectId } })
      if (response.error) {
        throw new Error(problemMessage(response.error, 'Failed to revoke service'))
      }
    },
    onSuccess: () => {
      setProjectToRevoke(null)
      queryClient.invalidateQueries({ queryKey: ['email-domain-projects', id] })
      toast.success('Service access revoked')
    },
    onError: (error: Error) => toast.error('Failed to revoke service', { description: error.message }),
  })

  const backAction = (
    <Button variant="ghost" size="sm" asChild>
      <Link to="/email?tab=domains">
        <ArrowLeft className="mr-2 size-4" />
        Back to domains
      </Link>
    </Button>
  )

  if (isLoading || isLoadingProviders || (isSmtp && isLoadingStoredDomains)) {
    return <EmailDomainDetailSkeleton backAction={backAction} />
  }

  if (fetchError && isVerifiedNotFound(fetchError)) {
    return (
      <PageState
        variant="empty"
        icon={Globe}
        title="Domain not found"
        description={`No email domain has ID ${idParam ?? ''}. It may have been deleted.`}
        action={backAction}
      />
    )
  }

  if (fetchError || !domain) {
    return (
      <PageState
        variant="failed"
        icon={AlertCircle}
        title="Couldn't load domain"
        description="This email domain may not exist, or you may not have permission to view it."
        action={<Button onClick={() => void refetchDomain()}>Retry</Button>}
      />
    )
  }

  const hasDnsProviders = dnsProviders && dnsProviders.length > 0
  const status = isSmtp
    ? storedDomains?.find((d) => d.id === domain.id)?.status
    : domain.status
  const isSmtpStatusUnknown = isSmtp && status === undefined
  const isVerified = status === 'verified'
  const noAuthorizedProjects =
    !isLoadingAuthorizations && !authorizationsError && authorizedProjects.length === 0
  // Advisory records (MX, DMARC) are excluded from the "N of M verified"
  // tally in the facts grid and card description, consistent with
  // DnsVerificationSummary and the backend's are_all_records_verified gate.
  const requiredDnsRecords = dnsRecords.filter(r => !isAdvisoryDnsRecord(r))
  const verifiedCount = requiredDnsRecords.filter(r => r.status === 'verified').length
  const totalCount = requiredDnsRecords.length
  const verdict = domainVerdict(status, isSmtp)

  return (
    <>
      <Detail
        title={<span className="font-mono">{domain.domain}</span>}
        verdict={<Status tone={verdict.tone} label={verdict.label} />}
        actions={
          <>
            {backAction}
            {!isSmtp && (
              <Button
                variant="outline"
                onClick={() => verifyMutation.mutate()}
                busy={verifyMutation.isPending}
                busyLabel="Verifying…"
              >
                <RefreshCw className="mr-2 size-4" />
                Verify DNS
              </Button>
            )}
            <Button
              variant="outline"
              className="text-destructive hover:text-destructive"
              onClick={() => setConfirmingDelete(true)}
            >
              <Trash2 className="mr-2 size-4" />
              Delete
            </Button>
          </>
        }
        facts={domainFacts(domain, provider, verifiedCount, totalCount, isSmtp)}
        main={
          <>
            <PublicIngressNotice
              title="Opens, clicks and delivery events will not be recorded"
            />
            {isSmtpStatusUnknown && (
              <Callout tone="error" title="Couldn't load whether sending is enabled">
                <p>
                  Emails from {domain.domain} may be captured instead of delivered. Reload the
                  domain&apos;s status to see whether sending is enabled.
                </p>
                <Button
                  size="sm"
                  variant="outline"
                  className="mt-2"
                  onClick={() => void refetchStoredDomains()}
                >
                  <RefreshCw className="mr-2 size-4" />
                  Retry
                </Button>
              </Callout>
            )}

            {isSmtp && !isSmtpStatusUnknown && !isVerified && (
              <Callout tone="warning" title="Sending is paused">
                <p>
                  Emails from {domain.domain} are captured, not delivered. Temps doesn&apos;t check DNS
                  for SMTP domains — make sure the domain is verified at your SMTP provider, then
                  enable sending.
                </p>
                <Button
                  size="sm"
                  className="mt-2"
                  onClick={() => verifyMutation.mutate()}
                  busy={verifyMutation.isPending}
                  busyLabel="Enabling…"
                >
                  Enable sending
                </Button>
              </Callout>
            )}

            {noAuthorizedProjects && (
              <Callout tone="warning" title="No project can send from this domain yet">
                <p>
                  Apps deployed on Temps send with their project&apos;s deployment token, which is
                  rejected for {domain.domain} until that project is authorized.
                  {!canManageAuthorizations
                    ? ' Ask an administrator to authorize one.'
                    : hasNoProjects
                      ? ' There are no projects yet — create one first.'
                      : ' Add one under Authorized projects.'}
                </p>
                {canManageAuthorizations &&
                  (hasNoProjects ? (
                    <Button size="sm" variant="outline" className="mt-2" asChild>
                      <Link to="/projects/new">
                        <Plus className="mr-2 size-4" />
                        Create a project
                      </Link>
                    </Button>
                  ) : (
                    <Button
                      size="sm"
                      variant="outline"
                      className="mt-2"
                      onClick={() => {
                        // On narrow screens the panel sits below the main
                        // column, so bring it into view before opening it.
                        document
                          .getElementById('authorized-projects')
                          ?.scrollIntoView({ behavior: 'smooth', block: 'center' })
                        setProjectPickerOpen(true)
                      }}
                    >
                      <KeyRound className="mr-2 size-4" />
                      Authorize a project
                    </Button>
                  ))}
              </Callout>
            )}

            {domain.verification_error ? (
              <Callout
                tone={domain.status === 'pending' ? 'warning' : 'error'}
                title={
                  domain.status === 'pending' ? 'Waiting for your email provider' : 'Verification error'
                }
              >
                <span className="break-all font-mono text-xs">{domain.verification_error}</span>
              </Callout>
            ) : null}

            {isLoadingStats ? (
              <StatsSkeleton />
            ) : statsError ? (
              <Callout tone="error" title="Failed to load email stats">
                <div className="flex items-center justify-between gap-3">
                  <span>
                    {statsError instanceof Error
                      ? statsError.message
                      : 'Could not fetch delivery stats for this domain.'}
                  </span>
                  <Button variant="outline" size="sm" onClick={() => void refetchStats()}>
                    Retry
                  </Button>
                </div>
              </Callout>
            ) : (
              emailStats && (
                <StatPanel
                  stats={[
                    { label: 'Total emails', value: emailStats.total },
                    { label: 'Sent', value: emailStats.sent },
                    { label: 'Captured', value: emailStats.captured },
                    { label: 'Queued', value: emailStats.queued },
                    { label: 'Sending', value: emailStats.sending },
                    { label: 'Delivery unknown', value: emailStats.delivery_unknown },
                    { label: 'Failed', value: emailStats.failed },
                  ]}
                />
              )
            )}

            {isSmtp ? (
              <Card>
                <CardHeader>
                  <CardTitle>DNS</CardTitle>
                  <CardDescription>
                    Temps doesn&apos;t manage or check DNS for SMTP domains. Configure SPF, DKIM and
                    DMARC for {domain.domain} at your SMTP provider
                    {provider ? ` (${provider.name})` : ''}, which verifies the domain before it
                    accepts mail.
                  </CardDescription>
                </CardHeader>
              </Card>
            ) : (
            <Card>
              <CardHeader>
                <div className="flex flex-wrap items-center justify-between gap-2">
                  <div className="space-y-1">
                    <CardTitle>DNS records</CardTitle>
                    <CardDescription>
                      {isVerified
                        ? 'Your domain is verified and ready to send email.'
                        : `${verifiedCount} of ${totalCount} records verified. Configure the records below in your DNS provider.`}
                    </CardDescription>
                  </div>
                </div>
              </CardHeader>
              <CardContent className="space-y-4">
                <DnsVerificationSummary
                  records={dnsRecords}
                  awaitingProvider={domain.status === 'pending'}
                />
                <DnsRecordsTable records={dnsRecords} />
              </CardContent>
            </Card>
            )}

            {!isSmtp && !isVerified && (
              <>
                {hasDnsProviders && (
                  <Card>
                    <CardHeader>
                      <CardTitle className="flex items-center gap-2">
                        <Wand2 className="size-5 text-primary" />
                        Automatic DNS setup
                      </CardTitle>
                      <CardDescription>
                        If you&apos;ve connected a DNS provider in Temps, we can create
                        these records for you.
                      </CardDescription>
                    </CardHeader>
                    <CardContent className="space-y-4">
                      <div className="flex flex-col gap-3 sm:flex-row">
                        <Select
                          value={selectedDnsProviderId?.toString() || ''}
                          onValueChange={(value) =>
                            setSelectedDnsProviderId(parseInt(value))
                          }
                        >
                          <SelectTrigger className="w-full sm:w-[280px]">
                            <SelectValue placeholder="Select DNS provider" />
                          </SelectTrigger>
                          <SelectContent>
                            {dnsProviders?.map((p) => (
                              <SelectItem key={p.id} value={p.id.toString()}>
                                <div className="flex items-center gap-2">
                                  <Settings2 className="size-4" />
                                  <span>{p.name}</span>
                                  <Badge variant="outline" className="ml-1 text-xs">
                                    {p.provider_type}
                                  </Badge>
                                </div>
                              </SelectItem>
                            ))}
                          </SelectContent>
                        </Select>
                        <Button
                          onClick={() => {
                            if (selectedDnsProviderId) {
                              setupDnsMutation.mutate({
                                dnsProviderId: selectedDnsProviderId,
                              })
                            }
                          }}
                          disabled={!selectedDnsProviderId}
                          busy={setupDnsMutation.isPending}
                          busyLabel="Setting up…"
                        >
                          <Wand2 className="mr-2 size-4" />
                          Setup automatically
                        </Button>
                      </div>

                      {dnsSetupResult && (
                        <div className="space-y-3">
                          <Separator />
                          <Callout tone={dnsSetupResult.success ? 'success' : 'error'} title={
                            dnsSetupResult.success ? 'DNS setup complete' : 'DNS setup incomplete'
                          }>
                            {dnsSetupResult.message}
                          </Callout>

                          <div className="space-y-2">
                            {dnsSetupResult.results.map((result, index) => (
                              <div
                                key={index}
                                className={cn(
                                  'flex items-center gap-2 rounded-md p-2 text-sm',
                                  result.success
                                    ? 'bg-success/10 text-success'
                                    : 'bg-destructive/10 text-destructive'
                                )}
                              >
                                {result.success ? (
                                  <CheckCircle2 className="size-4 shrink-0" />
                                ) : (
                                  <AlertCircle className="size-4 shrink-0" />
                                )}
                                <Badge variant="outline" className="text-xs">
                                  {result.record_type}
                                </Badge>
                                <span className="truncate font-mono text-xs">
                                  {result.name}
                                </span>
                                <span className="ml-auto text-xs">
                                  {result.message}
                                </span>
                              </div>
                            ))}
                          </div>
                        </div>
                      )}
                    </CardContent>
                  </Card>
                )}

                <Card>
                  <CardHeader>
                    <CardTitle>Manual DNS configuration</CardTitle>
                    <CardDescription>
                      Prefer to do it yourself? Add the records above in your DNS
                      provider, then click <span className="font-medium">Verify DNS</span>.
                    </CardDescription>
                  </CardHeader>
                  <CardContent>
                    <ol className="list-inside list-decimal space-y-1.5 text-sm text-muted-foreground">
                      <li>
                        Log in to your domain registrar or DNS provider (Cloudflare,
                        Route53, GoDaddy, etc.)
                      </li>
                      <li>Navigate to the DNS management section</li>
                      <li>Add each record shown above with the exact values</li>
                      <li>
                        Wait for DNS propagation (usually a few minutes, up to 48 hours)
                      </li>
                      <li>
                        Come back here and click <span className="font-medium">Verify DNS</span>
                      </li>
                    </ol>
                  </CardContent>
                </Card>
              </>
            )}
          </>
        }
        aside={
          <Card id="authorized-projects">
            <CardHeader>
              <CardTitle className="flex items-center gap-2">
                <KeyRound className="size-4" />
                Authorized services
              </CardTitle>
              <CardDescription>
                Choose which service deployment tokens may send email from {domain.domain}.
              </CardDescription>
            </CardHeader>
            <CardContent className="space-y-4">
              {canManageAuthorizations && !authorizationsError && (
                <div className="flex items-center gap-2">
                  <ProjectSelect
                    ariaLabel="Project to authorize"
                    open={projectPickerOpen}
                    onOpenChange={setProjectPickerOpen}
                    value={null}
                    onValueChange={(projectId) => {
                      if (projectId != null) setProjectToAuthorize(projectId)
                    }}
                    allowAll={false}
                    excludeIds={authorizedProjects.map((p) => p.id)}
                    placeholder="Search services by name or slug"
                    disabled={authorizeProjectMutation.isPending}
                    className="w-full sm:w-full"
                  />
                  {authorizeProjectMutation.isPending && (
                    <Loader2 className="size-4 shrink-0 animate-spin text-muted-foreground" />
                  )}
                </div>
              )}

              {isLoadingAuthorizations ? (
                <Skeleton className="h-16 w-full" />
              ) : authorizationsError ? (
                <Callout tone="error" title="Could not load service authorizations">
                  <div className="flex items-center justify-between gap-3">
                    <span>{authorizationsError.message}</span>
                    <Button variant="outline" size="sm" onClick={() => refetchAuthorizations()}>
                      Retry
                    </Button>
                  </div>
                </Callout>
              ) : authorizedProjects.length === 0 ? (
                <div className="rounded-md border border-dashed p-4 text-sm text-muted-foreground">
                  No services are authorized yet. Deployment tokens cannot send from this domain until you add one.
                </div>
              ) : (
                <div className="divide-y rounded-md border">
                  {authorizedProjects.map(project => (
                    <div key={project.id} className="flex items-center justify-between gap-3 p-3">
                      <div className="min-w-0">
                        <p className="truncate text-sm font-medium">{project.name}</p>
                        <p className="truncate font-mono text-xs text-muted-foreground">{project.slug}</p>
                      </div>
                      {canManageAuthorizations && <Button
                        variant="outline"
                        size="sm"
                        disabled={revokeProjectMutation.isPending}
                        onClick={() => setProjectToRevoke(project)}
                      >
                        Revoke
                      </Button>}
                    </div>
                  ))}
                </div>
              )}
              {!canManageAuthorizations && (
                <p className="text-sm text-muted-foreground">
                  Only an instance or platform administrator can change sender-domain service access.
                </p>
              )}
            </CardContent>
          </Card>
        }
      />

      <ConfirmDeleteDialog
        open={confirmingDelete}
        onOpenChange={setConfirmingDelete}
        resourceType="email domain"
        resourceName={domain.domain}
        confirmLabel="Delete domain"
        consequences="Its DNS configuration is removed from Temps (the DNS records in your registrar are not removed). Applications using this domain will no longer be able to send email."
        isPending={deleteMutation.isPending}
        onConfirm={() => deleteMutation.mutate()}
      />

      <ConfirmDeleteDialog
        open={projectToRevoke !== null}
        onOpenChange={(open) => {
          if (!open) setProjectToRevoke(null)
        }}
        resourceType="sender access for service"
        resourceName={projectToRevoke?.name ?? ''}
        verb="Revoke"
        confirmLabel="Revoke access"
        irreversible={false}
        consequences={`It will no longer be able to send email from ${domain.domain}. Existing deployments using this sender may begin failing immediately.`}
        isPending={revokeProjectMutation.isPending}
        onConfirm={() => {
          if (projectToRevoke) {
            revokeProjectMutation.mutate(projectToRevoke.id)
          }
        }}
      />

      <AlertDialog
        open={projectToAuthorize !== null}
        onOpenChange={(open) => {
          if (!open && !authorizeProjectMutation.isPending) {
            setProjectToAuthorize(null)
          }
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Authorize service to send email?</AlertDialogTitle>
            <AlertDialogDescription>
              <span className="font-medium text-foreground">
                {projectToAuthorizeName ?? 'This service'}
              </span>
              {' '}will be able to send email from{' '}
              <span className="font-medium text-foreground">{domain.domain}</span>.
              Every deployment token in that service gains this ability immediately.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={authorizeProjectMutation.isPending}>
              Cancel
            </AlertDialogCancel>
            <AlertDialogAction
              disabled={authorizeProjectMutation.isPending || projectToAuthorize === null}
              onClick={(event) => {
                event.preventDefault()
                if (projectToAuthorize !== null) {
                  authorizeProjectMutation.mutate(projectToAuthorize)
                }
              }}
            >
              {authorizeProjectMutation.isPending ? 'Authorizing...' : 'Authorize'}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </>
  )
}

export default EmailDomainDetail
