// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

'use client'

import {
  getEmailProvider,
  listEmailDomains,
  type EmailDomainResponse,
  type EmailProviderResponse,
} from '@/api/client'
import {
  EditProviderDialog,
  TestEmailDialog,
} from '@/components/email/EmailProvidersManagement'
import { StatusPill } from '@/components/email/EmailDomainsManagement'
import { EmailTrackingSetup } from '@/components/email/EmailTrackingSetup'
import {
  deleteEmailProvider,
  problemMessage,
} from '@/components/email/sharedUtils'
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
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import {
  EmailProviderLogo,
  getEmailProviderLabel,
  type EmailProviderType,
} from '@/components/ui/email-provider-logo'
import { EmptyState } from '@/components/ui/empty-state'
import { Skeleton } from '@/components/ui/skeleton'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import {
  Button,
  Callout,
  Detail,
  PageState,
  Status,
  fmtDateTime,
  fmtRelativeTime,
  type DetailFact,
  type StatusTone,
} from '@temps-sdk/ds'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { AlertCircle, ArrowLeft, Globe, Plus, Send, Trash2 } from 'lucide-react'
import { useEffect, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router'
import { toast } from 'sonner'

async function fetchProvider(id: number): Promise<EmailProviderResponse> {
  const response = await getEmailProvider({ path: { id } })
  if (response.error || !response.data) {
    const error = new Error(
      problemMessage(response.error, 'Failed to fetch email provider')
    ) as Error & { status?: number; title?: string }
    if (response.error) {
      error.status = (response.error as any).status
      error.title = (response.error as any).title
    }
    throw error
  }
  return response.data
}

async function fetchDomainsForProvider(
  providerId: number
): Promise<EmailDomainResponse[]> {
  const response = await listEmailDomains({
    query: { provider_id: providerId },
  })
  if (response.error) {
    throw new Error(problemMessage(response.error, 'Failed to fetch domains'))
  }
  return response.data ?? []
}

// Mirrors StatusPill/DOMAIN_STATUS_VERDICT tone-for-tone — the Detail
// template's single verdict, derived straight from is_active.
function providerVerdict(provider: { is_active: boolean }): {
  tone: StatusTone
  label: string
} {
  return provider.is_active
    ? { tone: 'ok', label: 'Active' }
    : { tone: 'idle', label: 'Inactive' }
}

function providerFacts(
  provider: EmailProviderResponse,
  domainsCount: number | undefined
): DetailFact[] {
  const facts: DetailFact[] = [
    {
      label: 'Type',
      value: (
        <span className="inline-flex items-center gap-1.5">
          <EmailProviderLogo
            provider={provider.provider_type as EmailProviderType}
            size={14}
          />
          {getEmailProviderLabel(provider.provider_type as EmailProviderType)}
        </span>
      ),
    },
    {
      label: 'Region',
      value: <span className="font-mono">{provider.region}</span>,
    },
  ]
  if (provider.provider_type === 'ses') {
    facts.push({
      label: 'SNS Topic ARN',
      value: provider.sns_topic_arn ? (
        <span className="break-all font-mono text-xs">
          {provider.sns_topic_arn}
        </span>
      ) : (
        'Not configured'
      ),
    })
  }
  facts.push(
    { label: 'Domains', value: domainsCount ?? 0 },
    {
      label: 'Created',
      value: (
        <span title={fmtDateTime(provider.created_at)}>
          {fmtRelativeTime(provider.created_at)}
        </span>
      ),
    },
    {
      label: 'Updated',
      value: (
        <span title={fmtDateTime(provider.updated_at)}>
          {fmtRelativeTime(provider.updated_at)}
        </span>
      ),
    }
  )
  return facts
}

function EmailProviderDetailSkeleton({
  backAction,
}: {
  backAction: React.ReactNode
}) {
  return (
    <Detail
      title={<Skeleton className="h-7 w-56" />}
      actions={backAction}
      facts={[0, 1, 2, 3].map(() => ({
        label: <Skeleton className="h-3 w-16" />,
        value: <Skeleton className="h-4 w-24" />,
      }))}
      main={
        <Card>
          <CardHeader>
            <Skeleton className="h-5 w-32" />
          </CardHeader>
          <CardContent className="space-y-3">
            <Skeleton className="h-14 w-full rounded-lg" />
            <Skeleton className="h-14 w-full rounded-lg" />
          </CardContent>
        </Card>
      }
    />
  )
}

export function EmailProviderDetail() {
  const { id: idParam } = useParams<{ id: string }>()
  const id = idParam ? parseInt(idParam, 10) : undefined
  const { setBreadcrumbs } = useBreadcrumbs()
  const queryClient = useQueryClient()
  const navigate = useNavigate()

  const [isEditDialogOpen, setIsEditDialogOpen] = useState(false)
  const [isTestDialogOpen, setIsTestDialogOpen] = useState(false)

  const {
    data: provider,
    isLoading,
    error: fetchError,
    refetch: refetchProvider,
  } = useQuery({
    queryKey: ['email-provider', id],
    queryFn: () => fetchProvider(id!),
    enabled: !!id,
  })

  useEffect(() => {
    if (provider && fetchError) {
      toast.error('Failed to refresh email provider', {
        action: { label: 'Retry', onClick: () => void refetchProvider() },
      })
    }
  }, [provider, fetchError, refetchProvider])

  const {
    data: domains,
    isLoading: isLoadingDomains,
    error: domainsError,
  } = useQuery({
    queryKey: ['email-domains', { provider_id: id }],
    queryFn: () => fetchDomainsForProvider(id!),
    enabled: !!id,
  })

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Email', href: '/email' },
      { label: 'Providers', href: '/email?tab=providers' },
      { label: provider?.name ?? 'Provider' },
    ])
  }, [setBreadcrumbs, provider?.name])

  usePageTitle(provider?.name ?? 'Email Provider')

  const deleteMutation = useMutation({
    mutationFn: () => deleteEmailProvider(id!),
    onSuccess: () => {
      toast.success('Email provider deleted')
      queryClient.invalidateQueries({ queryKey: ['email-providers'] })
      navigate('/email?tab=providers')
    },
    onError: (err: Error) => {
      toast.error('Failed to delete provider', { description: err.message })
    },
  })

  const backAction = (
    <Button variant="ghost" size="sm" asChild>
      <Link to="/email?tab=providers">
        <ArrowLeft className="mr-2 size-4" />
        Back to providers
      </Link>
    </Button>
  )

  if (isLoading) {
    return <EmailProviderDetailSkeleton backAction={backAction} />
  }

  const isNotFound =
    (fetchError as any)?.status === 404 ||
    (fetchError as any)?.title === 'Not Found' ||
    (fetchError as any)?.title === 'Provider Not Found'

  if (!provider && fetchError && !isNotFound) {
    return (
      <PageState
        variant="failed"
        icon={AlertCircle}
        title="Failed to load email provider"
        description={
          fetchError instanceof Error
            ? fetchError.message
            : 'An unexpected error occurred. Please try again.'
        }
        action={
          <div className="flex gap-2">
            <Button variant="outline" onClick={() => void refetchProvider()}>
              Retry
            </Button>
            {backAction}
          </div>
        }
      />
    )
  }

  if (!provider) {
    return (
      <PageState
        variant="empty"
        icon={AlertCircle}
        title="Provider not found"
        description="The requested email provider could not be found."
        action={backAction}
      />
    )
  }

  const verdict = providerVerdict(provider)

  return (
    <>
      <Detail
        title={provider.name}
        verdict={<Status tone={verdict.tone} label={verdict.label} />}
        actions={
          <>
            {backAction}
            <Button variant="outline" onClick={() => setIsTestDialogOpen(true)}>
              <Send className="mr-2 size-4" />
              Send Test Email
            </Button>
            <Button variant="outline" onClick={() => setIsEditDialogOpen(true)}>
              Edit
            </Button>
            <AlertDialog>
              <AlertDialogTrigger asChild>
                <Button
                  variant="outline"
                  className="text-destructive hover:text-destructive"
                >
                  <Trash2 className="mr-2 size-4" />
                  Delete
                </Button>
              </AlertDialogTrigger>
              <AlertDialogContent>
                <AlertDialogHeader>
                  <AlertDialogTitle>Delete {provider.name}?</AlertDialogTitle>
                  <AlertDialogDescription>
                    This will permanently delete the provider and its stored
                    credentials from Temps. Domains still assigned to this
                    provider will be unable to send email until reassigned.
                  </AlertDialogDescription>
                </AlertDialogHeader>
                <AlertDialogFooter>
                  <AlertDialogCancel>Cancel</AlertDialogCancel>
                  <AlertDialogAction
                    onClick={() => deleteMutation.mutate()}
                    className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
                  >
                    Delete provider
                  </AlertDialogAction>
                </AlertDialogFooter>
              </AlertDialogContent>
            </AlertDialog>
          </>
        }
        facts={providerFacts(provider, domains?.length)}
        main={
          <>
            {/* Domains using this provider */}
            <Card>
              <CardHeader>
                <div className="flex flex-wrap items-center justify-between gap-2">
                  <CardTitle>Domains</CardTitle>
                  <Button variant="outline" size="sm" asChild>
                    <Link to="/email?tab=domains">
                      <Plus className="mr-2 size-4" />
                      Add domain
                    </Link>
                  </Button>
                </div>
              </CardHeader>
              <CardContent>
                {isLoadingDomains ? (
                  <div className="space-y-2">
                    <Skeleton className="h-12 w-full" />
                    <Skeleton className="h-12 w-full" />
                  </div>
                ) : domainsError ? (
                  <Callout tone="error" title="Failed to load domains">
                    {domainsError instanceof Error
                      ? domainsError.message
                      : 'Could not fetch domains for this provider.'}
                  </Callout>
                ) : !domains || domains.length === 0 ? (
                  <EmptyState
                    icon={Globe}
                    title="No domains yet"
                    description="Add a sending domain and assign it to this provider to start sending email."
                    action={
                      <Button asChild size="sm">
                        <Link to="/email?tab=domains">
                          <Plus className="mr-2 size-4" />
                          Add domain
                        </Link>
                      </Button>
                    }
                  />
                ) : (
                  <ul role="list" className="divide-y rounded-lg border">
                    {domains.map((domain) => (
                      <li key={domain.id}>
                        <Link
                          to={`/email/domains/${domain.id}`}
                          className="flex items-center justify-between gap-4 px-4 py-3 transition-colors hover:bg-muted/40"
                        >
                          <div className="min-w-0">
                            <p className="truncate text-sm font-medium">
                              {domain.domain}
                            </p>
                            <p className="mt-1 text-xs text-muted-foreground">
                              Added{' '}
                              <span title={fmtDateTime(domain.created_at)}>
                                {fmtRelativeTime(domain.created_at)}
                              </span>
                            </p>
                          </div>
                          <StatusPill status={domain.status} />
                        </Link>
                      </li>
                    ))}
                  </ul>
                )}
              </CardContent>
            </Card>

            {/* Delivery event tracking — self-gates to SES providers only */}
            <EmailTrackingSetup providerId={provider.id} />
          </>
        }
      />

      <EditProviderDialog
        provider={provider}
        open={isEditDialogOpen}
        onOpenChange={setIsEditDialogOpen}
        onSuccess={() => {
          queryClient.invalidateQueries({ queryKey: ['email-provider', id] })
          queryClient.invalidateQueries({ queryKey: ['email-providers'] })
        }}
      />
      <TestEmailDialog
        open={isTestDialogOpen}
        onOpenChange={setIsTestDialogOpen}
        providerId={provider.id}
        onSuccess={() => {}}
      />
    </>
  )
}
