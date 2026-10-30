// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Which traffic is kept out of this project's analytics, and how much was.
 *
 * Datacenter exclusion and the bundled referrer-spam list are on by default;
 * the project's own referrer/IP/network/country filters are listed below
 * them. Datacenter exclusion depends on the ASN database being loaded on this
 * instance. The toggle always renders; when the database is missing the page
 * says that nothing is being detected, why, and links to the geolocation
 * settings -- a switch that silently does nothing would be worse than no
 * switch.
 */

import type {
  FilteredTrafficResponse,
  ProjectAnalyticsSettingsResponse,
  ProjectResponse,
  UpdateProjectAnalyticsSettingsRequest,
} from '@/api/client'
import {
  getFilteredTrafficOptions,
  getProjectAnalyticsSettingsOptions,
  getProjectAnalyticsSettingsQueryKey,
  updateProjectAnalyticsSettingsMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { PageHeader } from '@/components/layout/PageContainer'
import { ReadFailure } from '@/components/ui/read-failure'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Label } from '@/components/ui/label'
import { SettingsSection } from '@/components/ui/settings-section'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import { usePageTitle } from '@/hooks/usePageTitle'
import { problemDetail } from '@/lib/api-problem'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { format } from 'date-fns'
import {
  AlertTriangle,
  ArrowRight,
  BarChart3,
  Link2Off,
  Server,
} from 'lucide-react'
import { Link } from 'react-router'
import { toast } from 'sonner'
import { TrafficFiltersSection } from './TrafficFiltersSection'
import {
  FILTERED_TRAFFIC_REASON_LABELS,
  filteredShare,
} from './traffic-filters-shared'

interface AnalyticsTrafficSettingsProps {
  project: ProjectResponse
}

/** Window of the "filtered recently" summary, in days. */
const SUMMARY_DAYS = 7

/** Networks the exclusion covers, named so the user knows what it means. */
const EXAMPLE_NETWORKS =
  'AWS, Google Cloud, Azure, Alibaba Cloud, Tencent Cloud, Hetzner, DigitalOcean, OVH'

export function AnalyticsTrafficSettings({
  project,
}: AnalyticsTrafficSettingsProps) {
  usePageTitle(`${project.name} · Traffic filters`)
  const queryClient = useQueryClient()
  const options = getProjectAnalyticsSettingsOptions({
    path: { project_id: project.id },
  })
  const {
    data: settings,
    isPending,
    isFetching,
    error,
    refetch,
  } = useQuery(options)

  const summary = useQuery(
    getFilteredTrafficOptions({
      path: { project_id: project.id },
      query: { days: SUMMARY_DAYS },
    })
  )

  const update = useMutation({
    ...updateProjectAnalyticsSettingsMutation(),
    meta: { errorTitle: 'Failed to update traffic filters' },
    onSuccess: (saved, variables) => {
      queryClient.setQueryData(
        // Key by the project the save was for: the user may have switched
        // projects while it was in flight.
        getProjectAnalyticsSettingsQueryKey({
          path: { project_id: variables.path.project_id },
        }),
        saved
      )
      toast.success(settingsSavedMessage(saved, variables.body))
    },
  })

  const save = (body: UpdateProjectAnalyticsSettingsRequest) =>
    update.mutate({ path: { project_id: project.id }, body })

  return (
    <div className="space-y-6">
      <PageHeader
        title="Traffic filters"
        description={`Traffic kept out of ${project.name}'s analytics reports.`}
      />

      <FilteredTrafficSection
        days={SUMMARY_DAYS}
        isPending={summary.isPending}
        error={summary.error}
        data={summary.data}
        retrying={summary.isFetching}
        onRetry={() => void summary.refetch()}
      />

      {isPending ? (
        <TrafficSettingsSkeleton />
      ) : !settings ? (
        // Only when nothing loaded: a failed background refresh keeps the
        // switches already on screen.
        <Alert variant="destructive">
          <AlertTriangle className="h-4 w-4" />
          <AlertTitle>Could not load traffic filters</AlertTitle>
          <AlertDescription className="space-y-3">
            <p>
              {problemDetail(
                error,
                'This instance could not report the traffic filters for this project.'
              )}
            </p>
            <Button size="sm" variant="outline" onClick={() => void refetch()}>
              Try again
            </Button>
          </AlertDescription>
        </Alert>
      ) : (
        <>
          {/* A failed background refresh keeps the switches, and says so. */}
          {error && (
            <ReadFailure
              resource="Traffic filter settings"
              error={error}
              cached
              retrying={isFetching}
              onRetry={() => void refetch()}
            />
          )}
          <SettingsSection
            defaultOpen
            title="Datacenter & cloud traffic"
            description="Visits from servers rather than people: scrapers, headless browsers, uptime checks and bots that fake a browser user agent."
            icon={Server}
          >
            <div className="space-y-4">
              <div className="flex items-start justify-between gap-4">
                <div className="space-y-1">
                  <Label
                    htmlFor="exclude-datacenter-traffic"
                    className="text-sm font-medium"
                  >
                    Exclude datacenter traffic
                  </Label>
                  <p className="text-sm text-muted-foreground">
                    Visits from hosting and cloud networks ({EXAMPLE_NETWORKS},
                    …) are left out of visitors, pageviews, referrers and every
                    other report. iCloud Private Relay and Cloudflare WARP users
                    are always counted.
                  </p>
                </div>
                <Switch
                  id="exclude-datacenter-traffic"
                  checked={settings.exclude_datacenter_traffic}
                  disabled={update.isPending}
                  onCheckedChange={(checked) =>
                    save({ exclude_datacenter_traffic: checked })
                  }
                />
              </div>

              <DetectionStatus settings={settings} />

              <p className="text-xs text-muted-foreground">
                Applies to visits recorded from now on. Visits already recorded
                keep their classification. Speed reports are the exception: they
                check every sample when shown, so the setting applies to older
                samples too.
              </p>
            </div>
          </SettingsSection>

          <SettingsSection
            defaultOpen
            title="Referrer spam"
            description="Domains that exist only to plant fake referrals in analytics reports, so their name shows up in your Referrers list."
            icon={Link2Off}
          >
            <div className="flex items-start justify-between gap-4">
              <div className="space-y-1">
                <Label
                  htmlFor="exclude-spam-referrers"
                  className="text-sm font-medium"
                >
                  Exclude known referrer spam
                </Label>
                <p className="text-sm text-muted-foreground">
                  Visits referred by any of the{' '}
                  {settings.spam_referrer_list_size.toLocaleString()} domains on
                  the public, community-maintained referrer-spam list (and their
                  subdomains) are left out of every report. To exclude a domain
                  that is not on the list, add a referrer filter below or use{' '}
                  <span className="font-medium text-foreground">
                    Exclude from analytics
                  </span>{' '}
                  on the Referrers card.
                </p>
              </div>
              <Switch
                id="exclude-spam-referrers"
                checked={settings.exclude_spam_referrers}
                disabled={update.isPending}
                onCheckedChange={(checked) =>
                  save({ exclude_spam_referrers: checked })
                }
              />
            </div>
          </SettingsSection>
        </>
      )}

      <TrafficFiltersSection projectId={project.id} />
    </div>
  )
}

function settingsSavedMessage(
  saved: ProjectAnalyticsSettingsResponse,
  changed: UpdateProjectAnalyticsSettingsRequest
): string {
  if (changed.exclude_spam_referrers != null) {
    return saved.exclude_spam_referrers
      ? 'Known referrer spam is now excluded from analytics'
      : 'Known referrer spam is now counted in analytics'
  }
  return saved.exclude_datacenter_traffic
    ? 'Datacenter traffic is now excluded from analytics'
    : 'Datacenter traffic is now counted in analytics'
}

interface FilteredTrafficSectionProps {
  days: number
  isPending: boolean
  /** The last fetch's error; with `data` loaded, the counts are stale. */
  error: unknown
  data: FilteredTrafficResponse | undefined
  retrying: boolean
  onRetry: () => void
}

/**
 * What was kept out over the last few days and why, so the user can trust the
 * filters -- and spot one that is hiding real visitors.
 */
function FilteredTrafficSection({
  days,
  isPending,
  error,
  data,
  retrying,
  onRetry,
}: FilteredTrafficSectionProps) {
  return (
    <SettingsSection
      defaultOpen
      title={`Filtered in the last ${days} days`}
      description="Events left out of this project's analytics, by reason. Filtered events are kept, not deleted, so these numbers stay accurate."
      icon={BarChart3}
    >
      <FilteredTrafficBody
        isPending={isPending}
        error={error}
        data={data}
        retrying={retrying}
        onRetry={onRetry}
      />
    </SettingsSection>
  )
}

function FilteredTrafficBody({
  isPending,
  error,
  data,
  retrying,
  onRetry,
}: Omit<FilteredTrafficSectionProps, 'days'>) {
  if (isPending) {
    return (
      <div className="grid grid-cols-2 gap-3 md:grid-cols-4">
        {[0, 1, 2, 3].map((tile) => (
          <div key={tile} className="space-y-2 rounded-md border p-3">
            <Skeleton className="h-3 w-24" />
            <Skeleton className="h-6 w-16" />
          </div>
        ))}
      </div>
    )
  }

  if (!data) {
    return (
      <div className="flex flex-col items-start gap-2 text-sm">
        <p className="text-destructive">
          {problemDetail(error, 'Could not load the filtered-traffic summary.')}
        </p>
        <Button size="sm" variant="outline" onClick={onRetry}>
          Try again
        </Button>
      </div>
    )
  }

  const share = filteredShare(data.counted_events, data.filtered_events)
  return (
    <div className="space-y-3">
      {/* A failed background refresh keeps the counts, and says so. */}
      {error != null && (
        <ReadFailure
          resource="Filtered-traffic summary"
          error={error}
          cached
          retrying={retrying}
          onRetry={onRetry}
        />
      )}
      <div className="grid grid-cols-2 gap-3 md:grid-cols-4">
        <SummaryTile
          label="Counted"
          value={data.counted_events.toLocaleString()}
        />
        <SummaryTile
          label="Filtered"
          value={data.filtered_events.toLocaleString()}
          hint={share ? `${share} of all events` : undefined}
        />
        {data.reasons.map((reason) => (
          <SummaryTile
            key={reason.reason}
            label={FILTERED_TRAFFIC_REASON_LABELS[reason.reason]}
            value={reason.events.toLocaleString()}
          />
        ))}
      </div>
      {data.counted_events + data.filtered_events === 0 && (
        <p className="text-sm text-muted-foreground">
          No events were recorded in this period.
        </p>
      )}
    </div>
  )
}

function SummaryTile({
  label,
  value,
  hint,
}: {
  label: string
  value: string
  hint?: string
}) {
  return (
    <div className="rounded-md border p-3">
      <p className="text-xs text-muted-foreground">{label}</p>
      <p className="text-lg font-semibold tabular-nums">{value}</p>
      {hint && <p className="text-xs text-muted-foreground">{hint}</p>}
    </div>
  )
}

function DetectionStatus({
  settings,
}: {
  settings: ProjectAnalyticsSettingsResponse
}) {
  const detection = settings.datacenter_detection
  if (detection.available) {
    return (
      <div className="flex flex-wrap items-center gap-2 text-sm text-muted-foreground">
        <Badge variant="secondary">Detection active</Badge>
        {detection.asn_database_built_at && (
          <span>
            Network database from{' '}
            {format(new Date(detection.asn_database_built_at), 'MMM d, yyyy')}
          </span>
        )}
      </div>
    )
  }

  return (
    <Alert>
      <AlertTriangle className="h-4 w-4" />
      <AlertTitle>Datacenter traffic is not being detected</AlertTitle>
      <AlertDescription className="space-y-3">
        <p>
          {detection.reason ??
            'No ASN database is loaded, so visits cannot be classified by network.'}{' '}
          Until it is, this setting has no effect.
        </p>
        {detection.setup_path && (
          <Button asChild size="sm" variant="outline">
            <Link to={detection.setup_path}>
              Open geolocation settings
              <ArrowRight className="ml-2 h-4 w-4" />
            </Link>
          </Button>
        )}
      </AlertDescription>
    </Alert>
  )
}

function TrafficSettingsSkeleton() {
  return (
    <div className="space-y-3 rounded-lg border p-6">
      <Skeleton className="h-5 w-56" />
      <Skeleton className="h-4 w-full max-w-xl" />
      <div className="flex items-center justify-between gap-4 pt-2">
        <Skeleton className="h-4 w-72" />
        <Skeleton className="h-6 w-11 rounded-full" />
      </div>
    </div>
  )
}
