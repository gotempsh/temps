// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getTelemetrySettingsOptions,
  getTelemetrySettingsQueryKey,
  updateTelemetrySettingsMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type { TelemetryStatusResponse } from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import { CopyButton } from '@/components/ui/copy-button'
import { Label } from '@/components/ui/label'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import {
  cacheSavedTelemetryPreference,
  groupTelemetryEvents,
  summarizeTelemetryState,
  telemetryToggleState,
} from '@/lib/telemetry-settings'
import { Callout, PageHeader, SettingsGroup, Status } from '@temps-sdk/ds'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { ExternalLink, RefreshCw } from 'lucide-react'
import { useEffect } from 'react'
import { Link } from 'react-router'
import { toast } from 'sonner'

const PAYLOAD_FIELDS: { field: string; description: string }[] = [
  {
    field: 'anonymous_id',
    description:
      'Random ID generated on this server. Not derived from your hardware, domain, account or instance name.',
  },
  {
    field: 'event_type',
    description: 'One of the event names listed below.',
  },
  {
    field: 'temps_version',
    description: 'The Temps version running on this server.',
  },
  {
    field: 'properties',
    description:
      'Counts (projects, environments, services, nodes), enum labels (build preset, source type, engine), coarse bands (RAM tier, duration, size) and fixed failure codes. Never free-form text.',
  },
]

const NEVER_SENT =
  'Emails, IP addresses, repository names, domains, URLs, environment variables, error messages, stack traces, file paths from your projects, or anything you typed.'

function errorMessage(error: unknown): string {
  if (error && typeof error === 'object' && 'detail' in error) {
    const detail = (error as { detail?: unknown }).detail
    if (typeof detail === 'string' && detail) return detail
  }
  return error instanceof Error ? error.message : 'Unknown error'
}

function StatusSection({
  status,
  isSaving,
  onChange,
}: {
  status: TelemetryStatusResponse
  isSaving: boolean
  onChange: (enabled: boolean) => void
}) {
  const summary = summarizeTelemetryState(status)
  const toggle = telemetryToggleState(status, isSaving)
  return (
    <SettingsGroup
      title="Status"
      description="Whether this server reports anonymous usage, and what decided it."
    >
      <div className="flex flex-wrap items-center gap-2">
        <Status tone={summary.tone} label={summary.label} />
        <p className="text-sm text-muted-foreground">{summary.reason}</p>
      </div>

      {status.source === 'environment' ? (
        <Callout tone="info" title={`Forced off by ${status.env_var}`}>
          The server was started with <code>{status.env_var}=0</code> (or{' '}
          <code>false</code>/<code>off</code>/<code>no</code>/
          <code>disabled</code>). Nothing is sent, and that wins over this page.
          Any choice saved here applies only after the variable is removed and
          the server restarted.
        </Callout>
      ) : null}

      <div className="flex items-start justify-between gap-4 rounded-lg border p-3">
        <div className="min-w-0 space-y-0.5">
          <Label htmlFor="telemetry-enabled" className="text-sm">
            Saved preference for anonymous usage
          </Label>
          <p className="max-w-prose text-xs text-muted-foreground">
            {toggle.disabledReason ??
              (status.source === 'environment'
                ? 'Saved for when the host opt-out is removed. Nothing is sent while the override is set.'
                : 'Takes effect immediately. Every change is recorded in the audit log.')}
          </p>
        </div>
        <Switch
          id="telemetry-enabled"
          checked={toggle.checked}
          disabled={toggle.disabled}
          onCheckedChange={onChange}
        />
      </div>

      <dl className="grid gap-3 text-sm sm:grid-cols-[max-content_minmax(0,1fr)] sm:gap-x-6">
        <dt className="text-muted-foreground">Instance ID</dt>
        <dd className="flex min-w-0 items-center gap-1">
          {status.anonymous_id ? (
            <>
              <code className="truncate font-mono text-xs">
                {status.anonymous_id}
              </code>
              <CopyButton
                value={status.anonymous_id}
                minimal
                label="Copy instance ID"
              />
            </>
          ) : (
            <span className="text-muted-foreground">Not available</span>
          )}
        </dd>
        <dt className="text-muted-foreground">Sent to</dt>
        <dd className="min-w-0 font-mono text-xs">
          {status.endpoint_host ?? 'Not available'}
        </dd>
        {status.temps_version ? (
          <>
            <dt className="text-muted-foreground">Reported version</dt>
            <dd className="min-w-0 font-mono text-xs">
              {status.temps_version}
            </dd>
          </>
        ) : null}
      </dl>
    </SettingsGroup>
  )
}

function DisclosureSection({ status }: { status: TelemetryStatusResponse }) {
  const groups = groupTelemetryEvents(status.events)
  return (
    <>
      <SettingsGroup
        title="What is sent"
        description="Every event has exactly these fields."
      >
        <div className="rounded-lg border bg-card text-card-foreground">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead className="w-40">Field</TableHead>
                <TableHead>Contents</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {PAYLOAD_FIELDS.map((row) => (
                <TableRow key={row.field}>
                  <TableCell className="align-top font-mono text-xs">
                    {row.field}
                  </TableCell>
                  <TableCell className="whitespace-normal text-sm">
                    {row.description}
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
        <p className="text-sm text-muted-foreground">
          <span className="font-medium text-foreground">Never sent: </span>
          {NEVER_SENT}
        </p>
        <p className="text-sm text-muted-foreground">
          Like any HTTPS request, the receiving server sees this server&apos;s
          public IP address. It derives a two-letter country code from it and
          does not store the IP.
        </p>
        <p className="text-sm text-muted-foreground">
          This is separate from{' '}
          <Link to="/settings/cloud" className="underline underline-offset-4">
            Temps Cloud
          </Link>{' '}
          and from the OpenTelemetry data your own apps send to this server,
          which never leaves it.
        </p>
      </SettingsGroup>

      <SettingsGroup
        title="Events"
        description={`The ${status.events.length} event types this version of Temps can send.`}
      >
        <div className="rounded-lg border bg-card text-card-foreground">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead className="w-48">Area</TableHead>
                <TableHead>Events</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {groups.map((group) => (
                <TableRow key={group.category}>
                  <TableCell className="align-top text-sm font-medium">
                    {group.label}
                  </TableCell>
                  <TableCell className="whitespace-normal">
                    <ul className="flex flex-wrap gap-x-3 gap-y-1">
                      {group.events.map((name) => (
                        <li key={name} className="font-mono text-xs">
                          {name}
                        </li>
                      ))}
                    </ul>
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
        <a
          href={status.privacy_doc_url}
          target="_blank"
          rel="noopener noreferrer"
          className="inline-flex items-center gap-1 text-sm underline underline-offset-4"
        >
          Read the privacy documentation
          <ExternalLink className="size-3.5" aria-hidden="true" />
        </a>
      </SettingsGroup>
    </>
  )
}

function LoadingState() {
  return (
    <div className="space-y-10" aria-busy="true" aria-label="Loading telemetry">
      {[0, 1].map((key) => (
        <div
          key={key}
          className="grid gap-5 md:grid-cols-[minmax(0,1fr)_minmax(0,2fr)] md:gap-10"
        >
          <div className="space-y-2">
            <Skeleton className="h-5 w-24" />
            <Skeleton className="h-4 w-48" />
          </div>
          <div className="space-y-3">
            <Skeleton className="h-6 w-64" />
            <Skeleton className="h-16 w-full" />
            <Skeleton className="h-10 w-full" />
          </div>
        </div>
      ))}
    </div>
  )
}

export function TelemetrySettingsPage() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const queryClient = useQueryClient()

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Settings', href: '/settings' },
      { label: 'Telemetry' },
    ])
  }, [setBreadcrumbs])

  usePageTitle('Telemetry')

  const statusQuery = useQuery({
    ...getTelemetrySettingsOptions(),
    retry: false,
    refetchInterval: 30_000,
    refetchOnWindowFocus: true,
  })

  const update = useMutation({
    ...updateTelemetrySettingsMutation(),
    onMutate: () =>
      queryClient.cancelQueries({ queryKey: getTelemetrySettingsQueryKey() }),
    onSuccess: (data) => cacheSavedTelemetryPreference(queryClient, data),
    onError: (error) => {
      toast.error(`Telemetry setting not saved: ${errorMessage(error)}`)
    },
  })

  return (
    <div className="w-full min-w-0 space-y-6">
      <PageHeader
        title="Telemetry"
        description="Anonymous usage reporting that helps the Temps maintainers see whether self-hosted instances work. Your application data never leaves this server."
      />
      <div className="max-w-5xl space-y-10">
        {statusQuery.isPending ? <LoadingState /> : null}
        {statusQuery.isError ? (
          <Callout tone="error" title="Could not load the telemetry setting">
            <p>{errorMessage(statusQuery.error)}</p>
            <Button
              variant="outline"
              size="sm"
              className="mt-2"
              onClick={() => void statusQuery.refetch()}
            >
              <RefreshCw className="size-4" aria-hidden="true" />
              Retry
            </Button>
          </Callout>
        ) : null}
        {statusQuery.data ? (
          <>
            <StatusSection
              status={statusQuery.data}
              isSaving={update.isPending}
              onChange={(enabled) => update.mutate({ body: { enabled } })}
            />
            <DisclosureSection status={statusQuery.data} />
          </>
        ) : null}
      </div>
    </div>
  )
}
