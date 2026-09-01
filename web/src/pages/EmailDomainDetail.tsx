// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

'use client'

import {
  deleteEmailDomain,
  getDomain,
  getEmailStats,
  getProjects,
  listEmailDomainProjects,
  authorizeEmailDomainProject,
  revokeEmailDomainProject,
  listDnsProviders,
  listEmailProviders,
  setupDns,
  verifyDomain,
  type DnsProviderResponse,
  type EmailDomainWithDnsResponse,
  type EmailProviderResponse,
  type EmailStatsResponse,
  type AuthorizedEmailDomainProjectResponse,
  type ProjectResponse,
  type SetupDnsResponse,
} from '@/api/client'
import {
  DnsRecordsTable,
  DnsVerificationSummary,
  StatusPill,
} from '@/components/email/EmailDomainsManagement'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from '@/components/ui/alert-dialog'
import {
  EmailProviderLogo,
  getEmailProviderLabel,
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
import { Input } from '@/components/ui/input'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { useAuth } from '@/contexts/AuthContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useDebounce } from '@/hooks/useDebounce'
import { cn } from '@/lib/utils'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  AlertCircle,
  ArrowLeft,
  CheckCircle2,
  Globe,
  KeyRound,
  Loader2,
  RefreshCw,
  Settings2,
  Trash2,
  Wand2,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router'
import { toast } from 'sonner'
import { problemMessage } from '@/components/email/sharedUtils'

async function fetchDomain(id: number): Promise<EmailDomainWithDnsResponse> {
  const response = await getDomain({ path: { id } })
  if (response.error || !response.data) {
    throw new Error(problemMessage(response.error, 'Failed to fetch email domain'))
  }
  return response.data
}

async function fetchProviders(): Promise<EmailProviderResponse[]> {
  const response = await listEmailProviders()
  if (response.error) {
    throw new Error(problemMessage(response.error, 'Failed to fetch email providers'))
  }
  return response.data ?? []
}

async function fetchDnsProviders(): Promise<DnsProviderResponse[]> {
  const response = await listDnsProviders()
  if (response.error) {
    throw new Error(problemMessage(response.error, 'Failed to fetch DNS providers'))
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
    throw new Error(problemMessage(response.error, 'Failed to fetch email stats'))
  }
  return response.data
}

async function fetchAuthorizedProjects(domainId: number): Promise<AuthorizedEmailDomainProjectResponse[]> {
  const response = await listEmailDomainProjects({ path: { id: domainId } })
  if (response.error) {
    throw new Error(problemMessage(response.error, 'Failed to fetch authorized projects'))
  }
  return response.data ?? []
}

async function fetchProjects(search: string): Promise<ProjectResponse[]> {
  const response = await getProjects({
    query: { page: 1, per_page: 25, search: search.trim() || undefined },
  })
  if (response.error || !response.data) {
    throw new Error(problemMessage(response.error, 'Failed to fetch projects'))
  }
  return response.data.projects
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
              {stat.value.toLocaleString()}
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
  const [selectedProjectId, setSelectedProjectId] = useState<string>('')
  const [projectSearch, setProjectSearch] = useState('')
  const debouncedProjectSearch = useDebounce(projectSearch, 300)
  const [projectToRevoke, setProjectToRevoke] = useState<AuthorizedEmailDomainProjectResponse | null>(null)

  const {
    data: domainDetails,
    isLoading,
    error: fetchError,
  } = useQuery({
    queryKey: ['email-domain', id],
    queryFn: () => fetchDomain(id!),
    enabled: !!id,
  })

  const {
    data: emailStats,
    isLoading: isLoadingStats,
    error: statsError,
  } = useQuery({
    queryKey: ['email-stats', id],
    queryFn: () => fetchEmailStats(id!),
    enabled: !!id,
  })

  const { data: providers } = useQuery({
    queryKey: ['email-providers'],
    queryFn: fetchProviders,
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

  const {
    data: allProjects = [],
    isLoading: isLoadingProjects,
    error: projectsError,
    refetch: refetchProjects,
  } = useQuery({
    queryKey: ['projects', 'email-domain-authorization', debouncedProjectSearch],
    queryFn: () => fetchProjects(debouncedProjectSearch),
    enabled: canManageAuthorizations,
  })

  const domain = domainDetails?.domain
  const dnsRecords = domainDetails?.dns_records ?? []
  const provider = providers?.find((p) => p.id === domain?.provider_id)
  const availableProjects = allProjects.filter(
    project => !authorizedProjects.some(authorized => authorized.id === project.id),
  )

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
      const verifiedCount = data.dns_records.filter(r => r.status === 'verified').length
      const totalCount = data.dns_records.length
      const pendingCount = data.dns_records.filter(r => r.status === 'pending').length
      const failedCount = data.dns_records.filter(r => r.status === 'failed').length

      if (data.domain.status === 'verified') {
        toast.success('Domain verified', {
          description: `All ${totalCount} DNS records are properly configured.`,
        })
      } else if (failedCount > 0) {
        toast.error('Some DNS records failed verification', {
          description: `${failedCount} of ${totalCount} records failed.`,
        })
      } else if (pendingCount > 0) {
        toast.warning('Verification in progress', {
          description: `${verifiedCount} of ${totalCount} records verified. DNS propagation can take up to 48 hours.`,
        })
      } else {
        toast.info('Verification status updated', {
          description: `${verifiedCount} of ${totalCount} records verified.`,
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

  const authorizeProjectMutation = useMutation({
    mutationFn: async (projectId: number) => {
      const response = await authorizeEmailDomainProject({ path: { id: id!, project_id: projectId } })
      if (response.error) {
        throw new Error(problemMessage(response.error, 'Failed to authorize project'))
      }
    },
    onSuccess: () => {
      setSelectedProjectId('')
      queryClient.invalidateQueries({ queryKey: ['email-domain-projects', id] })
      toast.success('Project authorized', {
        description: 'Deployments in this project can now send from this domain.',
      })
    },
    onError: (error: Error) => toast.error('Failed to authorize project', { description: error.message }),
  })

  const revokeProjectMutation = useMutation({
    mutationFn: async (projectId: number) => {
      const response = await revokeEmailDomainProject({ path: { id: id!, project_id: projectId } })
      if (response.error) {
        throw new Error(problemMessage(response.error, 'Failed to revoke project'))
      }
    },
    onSuccess: () => {
      setProjectToRevoke(null)
      queryClient.invalidateQueries({ queryKey: ['email-domain-projects', id] })
      toast.success('Project access revoked')
    },
    onError: (error: Error) => toast.error('Failed to revoke project', { description: error.message }),
  })

  if (isLoading) {
    return (
      <div className="flex-1 overflow-auto">
        <div className="space-y-6 sm:p-4 md:p-6">
          <Skeleton className="h-8 w-32" />
          <div className="flex flex-col gap-4 sm:flex-row sm:items-start sm:justify-between">
            <div className="flex min-w-0 items-start gap-3">
              <Skeleton className="size-11 shrink-0 rounded-md" />
              <div className="space-y-2">
                <Skeleton className="h-7 w-56" />
                <Skeleton className="h-4 w-72" />
              </div>
            </div>
            <div className="flex gap-2">
              <Skeleton className="h-10 w-28" />
              <Skeleton className="h-10 w-24" />
            </div>
          </div>
          <div className="grid gap-6 lg:grid-cols-3">
            <div className="space-y-6 lg:col-span-2">
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
            </div>
            <div className="space-y-6">
              <Card>
                <CardHeader>
                  <Skeleton className="h-5 w-24" />
                </CardHeader>
                <CardContent>
                  <div className="space-y-3">
                    {[1, 2, 3, 4, 5].map((i) => (
                      <div key={i} className="grid grid-cols-3 gap-3">
                        <Skeleton className="h-4 w-20" />
                        <Skeleton className="col-span-2 h-4 w-full" />
                      </div>
                    ))}
                  </div>
                </CardContent>
              </Card>
            </div>
          </div>
        </div>
      </div>
    )
  }

  if (fetchError || !domain) {
    return (
      <div className="flex-1 overflow-auto">
        <div className="flex flex-col items-center justify-center py-16 text-center">
          <h2 className="text-lg font-semibold">Domain not found</h2>
          <p className="mt-1 text-sm text-muted-foreground">
            The requested email domain could not be found.
          </p>
          <Button asChild className="mt-4">
            <Link to="/email?tab=domains">
              <ArrowLeft className="mr-2 size-4" />
              Back to domains
            </Link>
          </Button>
        </div>
      </div>
    )
  }

  const hasDnsProviders = dnsProviders && dnsProviders.length > 0
  const isVerified = domain.status === 'verified'
  const verifiedCount = dnsRecords.filter(r => r.status === 'verified').length
  const totalCount = dnsRecords.length

  return (
    <div className="flex-1 overflow-auto">
      <div className="space-y-6 sm:p-4 md:p-6">
        {/* Back link */}
        <Button variant="ghost" size="sm" asChild className="-ml-2 w-fit">
          <Link to="/email?tab=domains">
            <ArrowLeft className="mr-2 size-4" />
            Back to domains
          </Link>
        </Button>

        {/* Header — logo tile + domain + pills + actions, matches BackupDetail overview */}
        <div className="flex flex-col gap-4 sm:flex-row sm:items-start sm:justify-between">
          <div className="flex min-w-0 items-start gap-3">
            <div className="flex size-11 shrink-0 items-center justify-center rounded-md border bg-background">
              {provider ? (
                <EmailProviderLogo
                  provider={provider.provider_type as EmailProviderType}
                  size={22}
                />
              ) : (
                <Globe className="size-5 text-muted-foreground" />
              )}
            </div>
            <div className="min-w-0 space-y-1.5">
              <h1 className="truncate font-mono text-xl font-semibold sm:text-2xl">
                {domain.domain}
              </h1>
              <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-sm text-muted-foreground">
                <StatusPill status={domain.status} />
                {provider && (
                  <span className="inline-flex items-center gap-1.5">
                    <span className="text-foreground">
                      {getEmailProviderLabel(provider.provider_type as EmailProviderType)}
                    </span>
                    <Badge variant="outline" className="font-mono text-[10px] uppercase">
                      {provider.provider_type}
                    </Badge>
                  </span>
                )}
                <span className="hidden sm:inline" aria-hidden>·</span>
                <span>
                  Added <TimeAgo date={domain.created_at} />
                </span>
              </div>
            </div>
          </div>

          <div className="flex flex-wrap items-center gap-2">
            <Button
              variant="outline"
              onClick={() => verifyMutation.mutate()}
              disabled={verifyMutation.isPending}
            >
              {verifyMutation.isPending ? (
                <Loader2 className="mr-2 size-4 animate-spin" />
              ) : (
                <RefreshCw className="mr-2 size-4" />
              )}
              Verify DNS
            </Button>
            <AlertDialog>
              <AlertDialogTrigger asChild>
                <Button variant="outline" className="text-destructive hover:text-destructive">
                  <Trash2 className="mr-2 size-4" />
                  Delete
                </Button>
              </AlertDialogTrigger>
              <AlertDialogContent>
                <AlertDialogHeader>
                  <AlertDialogTitle>Delete {domain.domain}?</AlertDialogTitle>
                  <AlertDialogDescription>
                    This will permanently delete the domain and its DNS configuration
                    from Temps. The DNS records in your registrar are not removed.
                    Applications using this domain will no longer be able to send email.
                  </AlertDialogDescription>
                </AlertDialogHeader>
                <AlertDialogFooter>
                  <AlertDialogCancel>Cancel</AlertDialogCancel>
                  <AlertDialogAction
                    onClick={() => deleteMutation.mutate()}
                    className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
                  >
                    Delete domain
                  </AlertDialogAction>
                </AlertDialogFooter>
              </AlertDialogContent>
            </AlertDialog>
          </div>
        </div>

        {/* Verification error banner */}
        {domain.verification_error && (
          <Alert variant="destructive">
            <AlertCircle className="size-4" />
            <AlertTitle>Verification error</AlertTitle>
            <AlertDescription className="break-all font-mono text-xs">
              {domain.verification_error}
            </AlertDescription>
          </Alert>
        )}

        {/* Delivery status stats for this domain */}
        {isLoadingStats ? (
          <StatsSkeleton />
        ) : statsError ? (
          <Alert variant="destructive">
            <AlertCircle className="h-4 w-4" />
            <AlertTitle>Failed to load email stats</AlertTitle>
            <AlertDescription>
              {statsError instanceof Error
                ? statsError.message
                : 'Could not fetch delivery stats for this domain.'}
            </AlertDescription>
          </Alert>
        ) : (
          emailStats && (
            <StatPanel
              stats={[
                { label: 'Total Emails', value: emailStats.total },
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

        {/* Two-column layout: DNS setup on left, overview on right */}
        <div className="grid gap-6 lg:grid-cols-3">
          <div className="space-y-6 lg:col-span-2">
            {/* DNS summary + records */}
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
                <DnsVerificationSummary records={dnsRecords} />
                <DnsRecordsTable records={dnsRecords} />
              </CardContent>
            </Card>

            {/* Setup paths — only if not already verified */}
            {!isVerified && (
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
                          disabled={
                            !selectedDnsProviderId || setupDnsMutation.isPending
                          }
                        >
                          {setupDnsMutation.isPending ? (
                            <>
                              <Loader2 className="mr-2 size-4 animate-spin" />
                              Setting up…
                            </>
                          ) : (
                            <>
                              <Wand2 className="mr-2 size-4" />
                              Setup automatically
                            </>
                          )}
                        </Button>
                      </div>

                      {dnsSetupResult && (
                        <div className="space-y-3">
                          <Separator />
                          <Alert
                            variant={dnsSetupResult.success ? 'default' : 'destructive'}
                          >
                            {dnsSetupResult.success ? (
                              <CheckCircle2 className="size-4" />
                            ) : (
                              <AlertCircle className="size-4" />
                            )}
                            <AlertTitle>
                              {dnsSetupResult.success
                                ? 'DNS setup complete'
                                : 'DNS setup incomplete'}
                            </AlertTitle>
                            <AlertDescription>
                              {dnsSetupResult.message}
                            </AlertDescription>
                          </Alert>

                          <div className="space-y-2">
                            {dnsSetupResult.results.map((result, index) => (
                              <div
                                key={index}
                                className={cn(
                                  'flex items-center gap-2 rounded-md p-2 text-sm',
                                  result.success
                                    ? 'bg-emerald-50 text-emerald-700 dark:bg-emerald-950/30 dark:text-emerald-400'
                                    : 'bg-red-50 text-red-700 dark:bg-red-950/30 dark:text-red-400'
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

                {/* Manual setup instructions */}
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
          </div>

          {/* Right column — overview */}
          <div className="space-y-6">
            <Card>
              <CardHeader>
                <CardTitle>Overview</CardTitle>
              </CardHeader>
              <CardContent>
                <dl className="divide-y">
                  <Row label="Status">
                    <StatusPill status={domain.status} />
                  </Row>
                  <Row label="Provider">
                    {provider ? (
                      <div className="flex items-center gap-2">
                        <EmailProviderLogo
                          provider={provider.provider_type as EmailProviderType}
                          size={16}
                        />
                        <span>{provider.name}</span>
                      </div>
                    ) : (
                      <span className="text-muted-foreground">—</span>
                    )}
                  </Row>
                  <Row label="Records">
                    <span className="tabular-nums">
                      {verifiedCount} / {totalCount} verified
                    </span>
                  </Row>
                  <Row label="Last verified">
                    {domain.last_verified_at ? (
                      <TimeAgo date={domain.last_verified_at} />
                    ) : (
                      <span className="text-muted-foreground">Never</span>
                    )}
                  </Row>
                  <Row label="Added">
                    <TimeAgo date={domain.created_at} />
                  </Row>
                </dl>
              </CardContent>
            </Card>
            <Card>
              <CardHeader>
                <CardTitle className="flex items-center gap-2">
                  <KeyRound className="size-4" />
                  Authorized projects
                </CardTitle>
                <CardDescription>
                  Choose which project deployment tokens may send email from {domain.domain}.
                </CardDescription>
              </CardHeader>
              <CardContent className="space-y-4">
                {canManageAuthorizations && !authorizationsError && !projectsError && <div className="space-y-2">
                  <Input
                    aria-label="Search projects"
                    value={projectSearch}
                    onChange={event => {
                      setProjectSearch(event.target.value)
                      setSelectedProjectId('')
                    }}
                    placeholder="Search projects by name or slug"
                  />
                  <div className="flex gap-2">
                  <Select value={selectedProjectId} onValueChange={setSelectedProjectId}>
                    <SelectTrigger
                      aria-label="Project to authorize"
                      className="min-w-0 flex-1"
                      disabled={isLoadingProjects}
                    >
                      <SelectValue placeholder={isLoadingProjects ? 'Loading projects…' : 'Select a project'} />
                    </SelectTrigger>
                    <SelectContent>
                      {availableProjects.length === 0 ? (
                        <div className="px-2 py-6 text-center text-sm text-muted-foreground">
                          No matching projects
                        </div>
                      ) : (
                        availableProjects.map(project => (
                          <SelectItem key={project.id} value={String(project.id)}>
                            {project.name}
                          </SelectItem>
                        ))
                      )}
                    </SelectContent>
                  </Select>
                  <Button
                    variant="outline"
                    onClick={() => authorizeProjectMutation.mutate(Number(selectedProjectId))}
                    disabled={!selectedProjectId || authorizeProjectMutation.isPending}
                  >
                    {authorizeProjectMutation.isPending && <Loader2 className="mr-2 size-4 animate-spin" />}
                    Authorize
                  </Button>
                  </div>
                  <p className="text-xs text-muted-foreground">
                    {availableProjects.length === 0
                      ? 'No projects match this search or all matches are already authorized.'
                      : `Showing ${availableProjects.length} of up to 25 matches. Refine the search to find another project.`}
                  </p>
                </div>}

                {isLoadingAuthorizations ? (
                  <Skeleton className="h-16 w-full" />
                ) : authorizationsError ? (
                  <Alert variant="destructive">
                    <AlertCircle className="size-4" />
                    <AlertTitle>Could not load project authorizations</AlertTitle>
                    <AlertDescription className="flex items-center justify-between gap-3">
                      <span>{authorizationsError.message}</span>
                      <Button variant="outline" size="sm" onClick={() => refetchAuthorizations()}>
                        Retry
                      </Button>
                    </AlertDescription>
                  </Alert>
                ) : projectsError && canManageAuthorizations ? (
                  <Alert variant="destructive">
                    <AlertCircle className="size-4" />
                    <AlertTitle>Could not load available projects</AlertTitle>
                    <AlertDescription className="flex items-center justify-between gap-3">
                      <span>{projectsError.message}</span>
                      <Button variant="outline" size="sm" onClick={() => refetchProjects()}>
                        Retry
                      </Button>
                    </AlertDescription>
                  </Alert>
                ) : authorizedProjects.length === 0 ? (
                  <div className="rounded-md border border-dashed p-4 text-sm text-muted-foreground">
                    No projects are authorized yet. Deployment tokens cannot send from this domain until you add one.
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
                    Only an instance or platform administrator can change sender-domain project access.
                  </p>
                )}
              </CardContent>
            </Card>
          </div>
        </div>
      </div>

      <AlertDialog
        open={projectToRevoke !== null}
        onOpenChange={(open) => {
          if (!open && !revokeProjectMutation.isPending) {
            setProjectToRevoke(null)
          }
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Revoke project access?</AlertDialogTitle>
            <AlertDialogDescription>
              <span className="font-medium text-foreground">{projectToRevoke?.name}</span>
              {' '}will no longer be able to send email from{' '}
              <span className="font-medium text-foreground">{domain.domain}</span>.
              Existing deployments using this sender may begin failing immediately.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={revokeProjectMutation.isPending}>
              Keep access
            </AlertDialogCancel>
            <AlertDialogAction
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
              disabled={revokeProjectMutation.isPending || projectToRevoke === null}
              onClick={(event) => {
                event.preventDefault()
                if (projectToRevoke) {
                  revokeProjectMutation.mutate(projectToRevoke.id)
                }
              }}
            >
              {revokeProjectMutation.isPending ? 'Revoking...' : 'Revoke access'}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  )
}

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="grid grid-cols-3 gap-3 py-2.5 text-sm first:pt-0 last:pb-0">
      <dt className="font-medium">{label}</dt>
      <dd className="col-span-2 min-w-0 text-muted-foreground">{children}</dd>
    </div>
  )
}
