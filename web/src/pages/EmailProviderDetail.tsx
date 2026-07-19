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
  AlertDialogTrigger,
} from '@/components/ui/alert-dialog'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import {
  EmailProviderLogo,
  getEmailProviderLabel,
  type EmailProviderType,
} from '@/components/ui/email-provider-logo'
import { EmptyState } from '@/components/ui/empty-state'
import { Skeleton } from '@/components/ui/skeleton'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { AlertCircle, ArrowLeft, Globe, Plus, Send, Trash2 } from 'lucide-react'
import { useEffect, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import { toast } from 'sonner'

async function fetchProvider(id: number): Promise<EmailProviderResponse> {
  const response = await getEmailProvider({ path: { id } })
  if (response.error || !response.data) {
    throw new Error(
      problemMessage(response.error, 'Failed to fetch email provider')
    )
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

function Row({
  label,
  children,
}: {
  label: string
  children: React.ReactNode
}) {
  return (
    <div className="grid grid-cols-3 gap-3 py-2.5 text-sm first:pt-0 last:pb-0">
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="col-span-2 min-w-0">{children}</dd>
    </div>
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
  } = useQuery({
    queryKey: ['email-provider', id],
    queryFn: () => fetchProvider(id!),
    enabled: !!id,
  })

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
                </CardHeader>
                <CardContent className="space-y-3">
                  <Skeleton className="h-14 w-full rounded-lg" />
                  <Skeleton className="h-14 w-full rounded-lg" />
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
                    {[1, 2, 3, 4].map((i) => (
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

  if (fetchError || !provider) {
    return (
      <div className="flex-1 overflow-auto">
        <div className="flex flex-col items-center justify-center py-16 text-center">
          <h2 className="text-lg font-semibold">Provider not found</h2>
          <p className="mt-1 text-sm text-muted-foreground">
            The requested email provider could not be found.
          </p>
          <Button asChild className="mt-4">
            <Link to="/email?tab=providers">
              <ArrowLeft className="mr-2 size-4" />
              Back to providers
            </Link>
          </Button>
        </div>
      </div>
    )
  }

  return (
    <div className="flex-1 overflow-auto">
      <div className="space-y-6 sm:p-4 md:p-6">
        {/* Back link */}
        <Button variant="ghost" size="sm" asChild className="-ml-2 w-fit">
          <Link to="/email?tab=providers">
            <ArrowLeft className="mr-2 size-4" />
            Back to providers
          </Link>
        </Button>

        {/* Header */}
        <div className="flex flex-col gap-4 sm:flex-row sm:items-start sm:justify-between">
          <div className="flex min-w-0 items-start gap-3">
            <div className="flex size-11 shrink-0 items-center justify-center rounded-md border bg-background">
              <EmailProviderLogo
                provider={provider.provider_type as EmailProviderType}
                size={22}
              />
            </div>
            <div className="min-w-0 space-y-1.5">
              <h1 className="truncate text-xl font-semibold sm:text-2xl">
                {provider.name}
              </h1>
              <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-sm text-muted-foreground">
                <Badge variant={provider.is_active ? 'default' : 'secondary'}>
                  {provider.is_active ? 'Active' : 'Inactive'}
                </Badge>
                <span className="inline-flex items-center gap-1.5">
                  <span className="text-foreground">
                    {getEmailProviderLabel(
                      provider.provider_type as EmailProviderType
                    )}
                  </span>
                  <Badge
                    variant="outline"
                    className="font-mono text-[10px] uppercase"
                  >
                    {provider.region}
                  </Badge>
                </span>
                <span className="hidden sm:inline" aria-hidden>
                  ·
                </span>
                <span>
                  Added <TimeAgo date={provider.created_at} />
                </span>
              </div>
            </div>
          </div>

          <div className="flex flex-wrap items-center gap-2">
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
          </div>
        </div>

        {/* Two-column layout: domains + tracking setup on left, overview on right */}
        <div className="grid gap-6 lg:grid-cols-3">
          <div className="space-y-6 lg:col-span-2">
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
                  <Alert variant="destructive">
                    <AlertCircle className="h-4 w-4" />
                    <AlertTitle>Failed to load domains</AlertTitle>
                    <AlertDescription>
                      {domainsError instanceof Error
                        ? domainsError.message
                        : 'Could not fetch domains for this provider.'}
                    </AlertDescription>
                  </Alert>
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
                              Added <TimeAgo date={domain.created_at} />
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
          </div>

          {/* Right column — overview */}
          <div className="space-y-6">
            <Card>
              <CardHeader>
                <CardTitle>Overview</CardTitle>
              </CardHeader>
              <CardContent>
                <dl className="divide-y">
                  <Row label="Type">
                    {getEmailProviderLabel(
                      provider.provider_type as EmailProviderType
                    )}
                  </Row>
                  <Row label="Region">
                    <span className="font-mono">{provider.region}</span>
                  </Row>
                  <Row label="Status">
                    <Badge
                      variant={provider.is_active ? 'default' : 'secondary'}
                    >
                      {provider.is_active ? 'Active' : 'Inactive'}
                    </Badge>
                  </Row>
                  {provider.provider_type === 'ses' && (
                    <Row label="SNS Topic ARN">
                      {provider.sns_topic_arn ? (
                        <span className="break-all font-mono text-xs">
                          {provider.sns_topic_arn}
                        </span>
                      ) : (
                        <span className="text-muted-foreground">
                          Not configured
                        </span>
                      )}
                    </Row>
                  )}
                  <Row label="Domains">
                    <span className="tabular-nums">{domains?.length ?? 0}</span>
                  </Row>
                  <Row label="Created">
                    <TimeAgo date={provider.created_at} />
                  </Row>
                  <Row label="Updated">
                    <TimeAgo date={provider.updated_at} />
                  </Row>
                </dl>
              </CardContent>
            </Card>
          </div>
        </div>
      </div>

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
    </div>
  )
}
