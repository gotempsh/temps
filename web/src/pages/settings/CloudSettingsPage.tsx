// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  disconnectCloudMutation,
  enrollCloudMutation,
  getCloudCapabilityOptions,
  getCloudStatusOptions,
  reconcileCloudBackupSourceMutation,
  updateCloudFeaturesMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type { ManagedBackupSetup } from '@/api/client/types.gen'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { getErrorMessage } from '@/utils/errorHandling'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  AlertCircle,
  Bell,
  Check,
  CheckCircle2,
  Cloud,
  DatabaseBackup,
  ExternalLink,
  Loader2,
  Radio,
  RefreshCw,
  ShieldCheck,
  Unplug,
} from 'lucide-react'
import { useEffect } from 'react'
import type { ReactNode } from 'react'
import { useForm } from 'react-hook-form'
import { toast } from 'sonner'
import { z } from 'zod'

const enrollmentSchema = z.object({
  enrollmentCode: z
    .string()
    .trim()
    .min(1, 'Paste the enrollment code from Temps Cloud'),
})

type EnrollmentForm = z.infer<typeof enrollmentSchema>

export function CloudSettingsPage() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const queryClient = useQueryClient()
  const capability = useQuery({
    ...getCloudCapabilityOptions(),
    retry: false,
  })
  const status = useQuery({
    ...getCloudStatusOptions(),
    refetchInterval: 5_000,
    retry: false,
  })
  const enroll = useMutation(enrollCloudMutation())
  const disconnect = useMutation(disconnectCloudMutation())
  const updateFeatures = useMutation(updateCloudFeaturesMutation())
  const reconcileBackupSource = useMutation(
    reconcileCloudBackupSourceMutation()
  )
  const form = useForm<EnrollmentForm>({
    resolver: zodResolver(enrollmentSchema),
    defaultValues: { enrollmentCode: '' },
  })

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Settings', href: '/settings' },
      { label: 'Temps Cloud' },
    ])
  }, [setBreadcrumbs])
  usePageTitle('Temps Cloud')

  const connected = status.data?.status === 'linked'
  const stateUnreadable = status.data?.status === 'state_unreadable'
  const degraded = connected && status.data?.health !== 'healthy'
  const cloudConsoleUrl = status.data?.backend_url ?? 'https://app.temps.sh'
  const cloudBillingUrl = `${cloudConsoleUrl.replace(/\/+$/, '')}/billing`
  const refresh = () =>
    Promise.all([
      queryClient.invalidateQueries({
        queryKey: getCloudStatusOptions().queryKey,
      }),
      queryClient.invalidateQueries({
        queryKey: getCloudCapabilityOptions().queryKey,
      }),
    ])

  const submit = form.handleSubmit(async ({ enrollmentCode }) => {
    try {
      await enroll.mutateAsync({ body: { enrollment_code: enrollmentCode } })
      form.reset()
      await refresh()
      toast.success('Instance connected to Temps Cloud')
    } catch (error) {
      toast.error(getErrorMessage(error, 'Could not connect this instance'))
    }
  })

  const remove = async () => {
    try {
      await disconnect.mutateAsync({})
      await refresh()
      toast.success('Temps Cloud disconnected')
    } catch (error) {
      toast.error(getErrorMessage(error, 'Could not disconnect this instance'))
    }
  }

  const setFeature = async (
    feature: 'telemetry_enabled' | 'backups_enabled' | 'notifications_enabled',
    enabled: boolean,
    label: string
  ) => {
    if (!status.data) return
    try {
      const updatedStatus = await updateFeatures.mutateAsync({
        body: {
          telemetry_enabled: status.data.telemetry_enabled,
          backups_enabled: status.data.backups_enabled,
          notifications_enabled: status.data.notifications_enabled,
          [feature]: enabled,
        },
      })
      queryClient.setQueryData(getCloudStatusOptions().queryKey, updatedStatus)
      toast.success(`${label} ${enabled ? 'enabled' : 'disabled'}`)
    } catch (error) {
      toast.error(
        getErrorMessage(error, `Could not update ${label.toLowerCase()}`)
      )
    }
  }

  const retryBackupSource = async () => {
    try {
      const managedBackupSetup = await reconcileBackupSource.mutateAsync({})
      if (status.data) {
        queryClient.setQueryData(getCloudStatusOptions().queryKey, {
          ...status.data,
          managed_backup_setup: managedBackupSetup,
        })
      }

      if (managedBackupSetup.ready) {
        toast.success('Managed backup source is ready')
      } else if (managedBackupSetup.action === 'renew_subscription') {
        toast.error('Renew the Cloud subscription to resume backup uploads')
      } else {
        toast.error(managedBackupSetup.message)
      }
    } catch (error) {
      toast.error(
        getErrorMessage(error, 'Could not retry managed backup setup')
      )
    }
  }

  if (status.isLoading || capability.isLoading) {
    return (
      <div className="mx-auto max-w-5xl space-y-6 pb-12">
        <Skeleton className="h-24 w-full" />
        <Skeleton className="h-64 w-full" />
      </div>
    )
  }

  if (status.isError || capability.isError) {
    const failedQuery = status.isError ? status : capability
    const title = status.isError
      ? 'Temps Cloud status unavailable'
      : 'Temps Cloud capability unavailable'
    return (
      <div className="mx-auto max-w-5xl pb-12">
        <Alert variant="destructive">
          <AlertCircle className="size-4" />
          <AlertTitle>{title}</AlertTitle>
          <AlertDescription className="space-y-3">
            <p>
              {getErrorMessage(
                failedQuery.error,
                'The server could not report whether Temps Cloud is available.'
              )}
            </p>
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() => void failedQuery.refetch()}
              disabled={failedQuery.isFetching}
            >
              {failedQuery.isFetching ? (
                <Loader2 className="animate-spin" />
              ) : null}
              Try again
            </Button>
          </AlertDescription>
        </Alert>
      </div>
    )
  }

  return (
    <div className="mx-auto max-w-5xl space-y-8 pb-12">
      <header className="border-b border-border pb-7">
        <div className="mb-3 flex items-center gap-2 font-mono text-[11px] uppercase tracking-[0.16em] text-muted-foreground">
          <Cloud className="size-3.5" /> Optional control plane
        </div>
        <h1 className="text-3xl font-semibold tracking-[-0.045em]">
          Temps Cloud
        </h1>
        <p className="mt-3 max-w-2xl text-sm leading-6 text-muted-foreground">
          See this instance alongside the rest of your fleet. Application
          traffic and primary telemetry storage stay on this machine.
        </p>
      </header>

      {!capability.data?.configured && (
        <Alert variant="destructive">
          <AlertCircle className="size-4" />
          <AlertTitle>Cloud connection needs configuration</AlertTitle>
          <AlertDescription>
            {capability.data?.reason ??
              'The managed backend is not configured.'}
          </AlertDescription>
        </Alert>
      )}

      {degraded ? (
        <Alert variant="destructive">
          <AlertCircle className="size-4" />
          <AlertTitle>Cloud connection is degraded</AlertTitle>
          <AlertDescription className="space-y-3">
            <p>
              {status.data?.health_message ||
                'Temps Cloud is linked, but the latest health check did not succeed. Local traffic and storage continue normally.'}
            </p>
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() => void status.refetch()}
              disabled={status.isFetching}
            >
              {status.isFetching ? <Loader2 className="animate-spin" /> : null}
              Check again
            </Button>
          </AlertDescription>
        </Alert>
      ) : null}

      {connected ? (
        <div className="space-y-6">
          <Card className="overflow-hidden border-border shadow-none">
            <div className="grid border-b border-border bg-muted/20 md:grid-cols-[1fr_auto] md:items-center">
              <div className="p-6">
                <p className="mb-2 font-mono text-[10px] uppercase tracking-[0.15em] text-muted-foreground">
                  Connection state
                </p>
                <div className="flex items-center gap-3">
                  <span className="grid size-9 place-items-center rounded-full border border-emerald-500/30 bg-emerald-500/10 text-emerald-500">
                    <Check className="size-4" />
                  </span>
                  <div>
                    <h2 className="font-semibold">Connected</h2>
                    <p className="text-sm text-muted-foreground">
                      {status.data?.account_email
                        ? `Cloud account: ${status.data.account_email}`
                        : 'Cloud account unavailable — reconnect to refresh it'}
                    </p>
                    <p className="mt-0.5 text-xs text-muted-foreground">
                      {status.data?.status_message}
                    </p>
                  </div>
                </div>
              </div>
              <div className="border-t border-border p-6 md:border-l md:border-t-0">
                <Button
                  variant="outline"
                  onClick={remove}
                  disabled={disconnect.isPending}
                >
                  <Unplug /> Disconnect
                </Button>
              </div>
            </div>
            <CardContent className="grid gap-px bg-border p-0 sm:grid-cols-3">
              <StatusCell
                label="Mirror health"
                value={status.data?.health ?? 'unknown'}
                detail={status.data?.health_message ?? ''}
              />
              <StatusCell
                label="Instance ID"
                value={status.data?.instance_id?.slice(0, 12) ?? '—'}
                detail="Stable across reconnections"
                mono
              />
              <StatusCell
                label="Buffered spans"
                value={String(status.data?.spooled_spans ?? 0)}
                detail="Local storage remains primary"
                mono
              />
            </CardContent>
          </Card>

          <Card className="border-border shadow-none">
            <CardContent className="space-y-5 p-6">
              <div>
                <p className="font-mono text-[10px] uppercase tracking-[0.15em] text-muted-foreground">
                  Explicit data controls
                </p>
                <h2 className="mt-2 text-lg font-semibold">Cloud exports</h2>
                <p className="mt-1 text-sm text-muted-foreground">
                  Connecting does not export data. Enable only the Cloud
                  features you want this instance to use.
                </p>
              </div>

              {updateFeatures.isError ? (
                <Alert variant="destructive">
                  <AlertCircle className="size-4" />
                  <AlertTitle>Could not update Cloud exports</AlertTitle>
                  <AlertDescription>
                    {getErrorMessage(
                      updateFeatures.error,
                      'The server did not save the export choice. Try the switch again.'
                    )}
                  </AlertDescription>
                </Alert>
              ) : null}

              <div className="divide-y divide-border border-y border-border">
                <FeatureToggle
                  icon={<Radio />}
                  label="Export telemetry to Cloud"
                  description="Mirror privacy-filtered spans with opaque identifiers and no application-controlled attributes."
                  checked={status.data?.telemetry_enabled ?? false}
                  disabled={updateFeatures.isPending}
                  onCheckedChange={(enabled) =>
                    void setFeature(
                      'telemetry_enabled',
                      enabled,
                      'Cloud telemetry export'
                    )
                  }
                />
                <FeatureToggle
                  icon={<DatabaseBackup />}
                  label="Export backups to Cloud"
                  description="Upload eligible completed backup objects for managed recovery while keeping the local copy authoritative."
                  checked={status.data?.backups_enabled ?? false}
                  disabled={updateFeatures.isPending}
                  onCheckedChange={(enabled) =>
                    void setFeature(
                      'backups_enabled',
                      enabled,
                      'Cloud backup export'
                    )
                  }
                />
                {status.data?.managed_backup_setup ? (
                  <BackupSourceStatus
                    setup={status.data.managed_backup_setup}
                    cloudBillingUrl={cloudBillingUrl}
                    isRetrying={reconcileBackupSource.isPending}
                    onRetry={() => void retryBackupSource()}
                  />
                ) : null}
                <FeatureToggle
                  icon={<Bell />}
                  label="Send notifications through Cloud"
                  description="Allow notification providers configured for Temps Cloud to send alert payloads."
                  checked={status.data?.notifications_enabled ?? false}
                  disabled={updateFeatures.isPending}
                  onCheckedChange={(enabled) =>
                    void setFeature(
                      'notifications_enabled',
                      enabled,
                      'Cloud notification delivery'
                    )
                  }
                />
              </div>
            </CardContent>
          </Card>
        </div>
      ) : stateUnreadable ? (
        <Card className="border-destructive/40 shadow-none">
          <CardContent className="p-6 md:p-8">
            <Alert variant="destructive" className="border-0 p-0">
              <AlertCircle className="size-4" />
              <AlertTitle>Cloud credentials need recovery</AlertTitle>
              <AlertDescription className="space-y-3">
                <p>{status.data?.status_message}</p>
                <p>
                  Temps will not overwrite the existing credential file. Restore
                  the encryption key that created it, or back up and remove the
                  unreadable Cloud state file before reconnecting.
                </p>
              </AlertDescription>
            </Alert>
          </CardContent>
        </Card>
      ) : (
        <div className="grid gap-8 lg:grid-cols-[minmax(0,1fr)_280px]">
          <Card className="border-border shadow-none">
            <CardContent className="p-6 md:p-8">
              <div className="mb-8 flex items-start justify-between gap-4">
                <div>
                  <p className="mb-2 font-mono text-[10px] uppercase tracking-[0.15em] text-muted-foreground">
                    Two-step setup
                  </p>
                  <h2 className="text-xl font-semibold tracking-tight">
                    Connect this instance
                  </h2>
                </div>
                <span className="rounded-full border border-border px-2.5 py-1 font-mono text-[10px] text-muted-foreground">
                  ~30 sec
                </span>
              </div>
              <form onSubmit={submit} className="space-y-5">
                <div className="space-y-2">
                  <div className="flex items-center justify-between">
                    <Label htmlFor="enrollment-code">
                      1. Paste enrollment code
                    </Label>
                    <a
                      href={cloudConsoleUrl}
                      target="_blank"
                      rel="noreferrer"
                      className="text-xs text-muted-foreground underline underline-offset-4 hover:text-foreground"
                    >
                      Get a code
                    </a>
                  </div>
                  <Input
                    id="enrollment-code"
                    autoFocus
                    placeholder="ABCD-EFGH"
                    className="h-12 font-mono uppercase tracking-[0.12em]"
                    aria-invalid={Boolean(form.formState.errors.enrollmentCode)}
                    {...form.register('enrollmentCode')}
                  />
                  {form.formState.errors.enrollmentCode && (
                    <p className="text-xs text-destructive">
                      {form.formState.errors.enrollmentCode.message}
                    </p>
                  )}
                </div>
                <Button
                  type="submit"
                  className="h-11 w-full"
                  disabled={enroll.isPending || !capability.data?.configured}
                >
                  {enroll.isPending ? (
                    <Loader2 className="animate-spin" />
                  ) : (
                    <Radio />
                  )}{' '}
                  2. Connect
                </Button>
              </form>
              <p className="mt-5 text-xs leading-5 text-muted-foreground">
                The code is exchanged for an instance-only credential stored
                with owner-only file permissions. Connecting does not export
                data by itself. Telemetry mirroring, Cloud backup uploads, and
                Cloud notification delivery are separate opt-ins and remain off
                until you enable them. When telemetry mirroring is enabled,
                spans contain opaque trace and span aliases, a neutral operation
                category, timestamp, and duration. Source code, environment
                variables, secrets, and application traffic never leave this
                instance.
              </p>
            </CardContent>
          </Card>

          <aside className="static h-auto border-0 bg-transparent p-0">
            <ol className="space-y-5">
              <TrustItem icon={<ShieldCheck />} title="Local stays primary">
                A Cloud outage never blocks ingest, deploys, or traffic.
              </TrustItem>
              <TrustItem icon={<Radio />} title="Background mirror">
                Accepted spans move through a bounded, non-blocking queue.
              </TrustItem>
              <TrustItem icon={<AlertCircle />} title="Visible failure">
                Buffering, dropped mirror data, and rejected credentials are
                explicit.
              </TrustItem>
            </ol>
          </aside>
        </div>
      )}
    </div>
  )
}

function BackupSourceStatus({
  setup,
  cloudBillingUrl,
  isRetrying,
  onRetry,
}: {
  setup: ManagedBackupSetup
  cloudBillingUrl: string
  isRetrying: boolean
  onRetry: () => void
}) {
  const subscriptionRequired = setup.status === 'subscription_required'
  const ready = setup.status === 'ready'
  const retryable =
    setup.status === 'needs_setup' || setup.status === 'unavailable'
  const alertVariant = subscriptionRequired
    ? 'warning'
    : retryable
      ? 'destructive'
      : 'default'

  let title = 'Managed backup source disabled'
  if (ready) title = 'Managed backup source ready'
  if (retryable) title = 'Managed backup source needs attention'
  if (subscriptionRequired) title = 'Cloud subscription required'

  return (
    <div className="py-4 pl-10" aria-live="polite">
      <Alert
        variant={alertVariant}
        className={
          ready
            ? 'border-emerald-500/30 bg-emerald-500/5 [&>svg]:text-emerald-500'
            : undefined
        }
      >
        {ready ? (
          <CheckCircle2 className="size-4" />
        ) : setup.status === 'disabled' ? (
          <DatabaseBackup className="size-4" />
        ) : (
          <AlertCircle className="size-4" />
        )}
        <AlertTitle>{title}</AlertTitle>
        <AlertDescription className="space-y-3">
          <p>{setup.message}</p>
          {subscriptionRequired ? (
            <div className="space-y-3">
              <p>
                Local backups remain available. Managed uploads resume after the
                Temps Cloud subscription is renewed.
              </p>
              <Button asChild size="sm">
                <a href={cloudBillingUrl} target="_blank" rel="noreferrer">
                  Renew subscription <ExternalLink className="size-3.5" />
                </a>
              </Button>
            </div>
          ) : null}
          {retryable || ready ? (
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={onRetry}
              disabled={isRetrying}
            >
              <RefreshCw className={isRetrying ? 'animate-spin' : undefined} />
              {ready ? 'Check again' : 'Retry setup'}
            </Button>
          ) : null}
        </AlertDescription>
      </Alert>
    </div>
  )
}

function StatusCell({
  label,
  value,
  detail,
  mono = false,
}: {
  label: string
  value: string
  detail: string
  mono?: boolean
}) {
  return (
    <div className="bg-card p-5">
      <p className="text-xs text-muted-foreground">{label}</p>
      <p
        className={`mt-2 text-sm font-semibold capitalize ${mono ? 'font-mono' : ''}`}
      >
        {value.split('_').join(' ')}
      </p>
      <p className="mt-1 text-[11px] leading-4 text-muted-foreground">
        {detail}
      </p>
    </div>
  )
}

function FeatureToggle({
  icon,
  label,
  description,
  checked,
  disabled,
  onCheckedChange,
}: {
  icon: ReactNode
  label: string
  description: string
  checked: boolean
  disabled: boolean
  onCheckedChange: (checked: boolean) => void
}) {
  return (
    <div className="grid grid-cols-[28px_minmax(0,1fr)_auto] items-start gap-3 py-4">
      <span className="mt-0.5 text-muted-foreground [&>svg]:size-4">
        {icon}
      </span>
      <div>
        <p className="text-sm font-medium">{label}</p>
        <p className="mt-1 text-xs leading-5 text-muted-foreground">
          {description}
        </p>
      </div>
      <Switch
        className="mt-0.5"
        checked={checked}
        disabled={disabled}
        onCheckedChange={onCheckedChange}
        aria-label={label}
      />
    </div>
  )
}

function TrustItem({
  icon,
  title,
  children,
}: {
  icon: ReactNode
  title: string
  children: ReactNode
}) {
  return (
    <li className="grid grid-cols-[28px_1fr] gap-3 border-t border-border pt-4 first:border-0 first:pt-0">
      <span className="mt-0.5 text-muted-foreground [&>svg]:size-4">
        {icon}
      </span>
      <div>
        <h3 className="text-sm font-medium">{title}</h3>
        <p className="mt-1 text-xs leading-5 text-muted-foreground">
          {children}
        </p>
      </div>
    </li>
  )
}
