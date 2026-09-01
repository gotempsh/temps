// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  revenueCreateIntegrationMutation,
  revenueDeleteIntegrationMutation,
  revenueListIntegrationsOptions,
  revenueListIntegrationsQueryKey,
  revenueListProvidersOptions,
  revenueMetricsCustomersOptions,
  revenueMetricsMrrOptions,
  revenueMetricsSummaryOptions,
  revenueRecentEventsOptions,
  revenueRotateTokenMutation,
  revenueUpdateConfigMutation,
  revenueUpdateSecretMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  CustomerMovementResponse,
  IntegrationResponse,
  LemonSqueezyConfig,
  MeteredMode,
  MrrBucketResponse,
  ProjectResponse,
  ProviderConfig,
  ProviderDescriptor,
  RecentEventResponse,
  StripeConfig,
} from '@/api/client/types.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import {
  ChartConfig,
  ChartContainer,
  ChartTooltip,
  ChartTooltipContent,
} from '@/components/ui/chart'
import { CopyButton } from '@/components/ui/copy-button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { EmptyState } from '@/components/ui/empty-state'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  ArrowDownRight,
  ArrowUpRight,
  CreditCard,
  KeyRound,
  Minus,
  MoreVertical,
  Plus,
  RefreshCw,
  Settings,
  Trash2,
  Upload,
} from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import {
  Area,
  AreaChart,
  Bar,
  BarChart,
  CartesianGrid,
  XAxis,
  YAxis,
} from 'recharts'
import { toast } from 'sonner'

interface ProjectRevenueProps {
  project: ProjectResponse
}

export function ProjectRevenue({ project }: ProjectRevenueProps) {
  const { setBreadcrumbs } = useBreadcrumbs()
  const [currency] = useState('usd')
  const [connectOpen, setConnectOpen] = useState(false)
  const [importOpen, setImportOpen] = useState(false)
  const [importTarget, setImportTarget] = useState<IntegrationResponse | null>(
    null,
  )

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Projects', href: '/projects' },
      { label: project.slug, href: `/projects/${project.slug}/project` },
      { label: 'Revenue' },
    ])
  }, [project.slug, setBreadcrumbs])

  const integrationsQuery = useQuery({
    ...revenueListIntegrationsOptions({ path: { project_id: project.id } }),
  })

  const integrations = integrationsQuery.data ?? []
  const hasIntegrations = integrations.length > 0
  const connectedProviders = useMemo(
    () => new Set(integrations.map((i) => i.provider)),
    [integrations],
  )

  const providersQuery = useQuery({
    ...revenueListProvidersOptions(),
  })
  const allProvidersConnected =
    (providersQuery.data?.length ?? 0) > 0 &&
    (providersQuery.data ?? []).every((p) => connectedProviders.has(p.name))

  const summaryQuery = useQuery({
    ...revenueMetricsSummaryOptions({ path: { project_id: project.id } }),
    enabled: hasIntegrations,
  })

  const mrrQuery = useQuery({
    ...revenueMetricsMrrOptions({ path: { project_id: project.id } }),
    enabled: hasIntegrations,
  })

  const customersQuery = useQuery({
    ...revenueMetricsCustomersOptions({ path: { project_id: project.id } }),
    enabled: hasIntegrations,
  })

  const eventsQuery = useQuery({
    ...revenueRecentEventsOptions({ path: { project_id: project.id } }),
    enabled: hasIntegrations,
  })

  return (
    <div className="flex w-full flex-col gap-6 p-2 sm:p-4">
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <div>
          <h1 className="text-xl font-semibold">Revenue</h1>
          <p className="text-sm text-muted-foreground">
            Track MRR, ARR, churn, and active customers for this project.
          </p>
        </div>
        {hasIntegrations && (
          <div className="flex flex-wrap items-center gap-2">
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button variant="outline" size="sm">
                  <Upload className="mr-2 h-4 w-4" />
                  Import CSV
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end">
                {integrations.map((i) => (
                  <DropdownMenuItem
                    key={i.id}
                    onClick={() => {
                      setImportTarget(i)
                      setImportOpen(true)
                    }}
                  >
                    <CreditCard className="mr-2 h-4 w-4" />
                    <span className="capitalize">{i.provider}</span>
                    {integrations.length > 1 && (
                      <span className="ml-2 text-xs text-muted-foreground">
                        #{i.id}
                      </span>
                    )}
                  </DropdownMenuItem>
                ))}
              </DropdownMenuContent>
            </DropdownMenu>
            {!allProvidersConnected && (
              <Button size="sm" onClick={() => setConnectOpen(true)}>
                <Plus className="mr-2 h-4 w-4" />
                Connect provider
              </Button>
            )}
          </div>
        )}
      </div>

      {integrationsQuery.isLoading ? (
        <SummarySkeleton />
      ) : !hasIntegrations ? (
        <ConnectEmptyState onConnect={() => setConnectOpen(true)} />
      ) : (
        <>
          <SummarySection
            currency={currency}
            isLoading={summaryQuery.isLoading}
            data={summaryQuery.data}
            customerBuckets={customersQuery.data ?? []}
            mrrBuckets={mrrQuery.data ?? []}
          />
          <div className="flex flex-col gap-4">
            {(mrrQuery.isLoading || (mrrQuery.data?.length ?? 0) >= 2) && (
              <MrrChart
                currency={currency}
                isLoading={mrrQuery.isLoading}
                buckets={mrrQuery.data ?? []}
              />
            )}
            {(customersQuery.isLoading ||
              (customersQuery.data?.length ?? 0) >= 2) && (
              <CustomersChart
                isLoading={customersQuery.isLoading}
                buckets={customersQuery.data ?? []}
              />
            )}
          </div>
          <IntegrationsSection
            projectId={project.id}
            integrations={integrationsQuery.data ?? []}
          />
          <RecentEventsSection
            isLoading={eventsQuery.isLoading}
            events={eventsQuery.data ?? []}
          />
        </>
      )}

      <ConnectProviderDialog
        projectId={project.id}
        open={connectOpen}
        onOpenChange={setConnectOpen}
        connectedProviders={connectedProviders}
      />
      {importTarget && (
        <ImportDataDialog
          projectId={project.id}
          integration={importTarget}
          open={importOpen}
          onOpenChange={(open) => {
            setImportOpen(open)
            if (!open) setImportTarget(null)
          }}
        />
      )}
    </div>
  )
}

function SummarySkeleton() {
  return (
    <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
      {Array.from({ length: 4 }).map((_, i) => (
        <Card key={i}>
          <CardContent className="p-4">
            <Skeleton className="mb-2 h-4 w-20" />
            <Skeleton className="h-7 w-24" />
          </CardContent>
        </Card>
      ))}
    </div>
  )
}

function ConnectEmptyState({ onConnect }: { onConnect: () => void }) {
  return (
    <EmptyState
      icon={CreditCard}
      title="Connect a payment provider"
      description="Paste a Temps-generated webhook URL into your provider's dashboard to start tracking MRR, ARR, churn, and LTV."
      action={
        <Button onClick={onConnect}>
          <Plus className="mr-2 h-4 w-4" />
          Connect provider
        </Button>
      }
    />
  )
}

type TrendDirection = 'up' | 'down' | 'flat'
type TrendTone = 'positive' | 'negative' | 'neutral'

type SummaryCard = {
  label: string
  value: string
  hint: string
  trend?: {
    direction: TrendDirection
    tone: TrendTone
    label: string
  }
}

function computeTrend(
  current: number,
  previous: number | undefined,
  options: {
    /** If true, a decrease is good (e.g. churn). */
    inverse?: boolean
    /** Formatter for the delta value. Defaults to `n.toLocaleString()`. */
    format?: (n: number) => string
  } = {},
): SummaryCard['trend'] | undefined {
  if (previous === undefined) return undefined
  const delta = current - previous
  const format = options.format ?? ((n) => n.toLocaleString())
  if (delta === 0) {
    return { direction: 'flat', tone: 'neutral', label: 'no change vs yesterday' }
  }
  const direction: TrendDirection = delta > 0 ? 'up' : 'down'
  const isGood = options.inverse ? delta < 0 : delta > 0
  const tone: TrendTone = isGood ? 'positive' : 'negative'
  const sign = delta > 0 ? '+' : '−'
  return {
    direction,
    tone,
    label: `${sign}${format(Math.abs(delta))} vs yesterday`,
  }
}

function SummarySection({
  currency,
  isLoading,
  data,
  customerBuckets,
  mrrBuckets,
}: {
  currency: string
  isLoading: boolean
  data?: {
    current_mrr_minor: number
    current_arr_minor: number
    active_subscriptions: number
    active_customers: number
    churned_last_30d: number
    arpu_minor: number
  }
  customerBuckets: CustomerMovementResponse[]
  mrrBuckets: MrrBucketResponse[]
}) {
  const cards = useMemo<SummaryCard[]>(() => {
    if (!data) return []

    const newLast30d = customerBuckets.reduce(
      (sum, b) => sum + b.new_customers,
      0,
    )

    // Previous-day comparisons come from the second-to-last bucket in each
    // series (buckets are ordered ASC by day). Falls back to `undefined` when
    // we don't have enough history yet — the trend chip simply hides.
    const prevMrrBucket =
      mrrBuckets.length >= 2 ? mrrBuckets[mrrBuckets.length - 2] : undefined
    const prevCustomerBucket =
      customerBuckets.length >= 2
        ? customerBuckets[customerBuckets.length - 2]
        : undefined
    const latestCustomerBucket =
      customerBuckets.length >= 1
        ? customerBuckets[customerBuckets.length - 1]
        : undefined

    const formatMoneyDelta = (n: number) => formatMinor(n, currency)

    return [
      {
        label: 'MRR',
        value: formatMinor(data.current_mrr_minor, currency),
        hint: 'Monthly recurring',
        trend: computeTrend(data.current_mrr_minor, prevMrrBucket?.mrr_minor, {
          format: formatMoneyDelta,
        }),
      },
      {
        label: 'ARR',
        value: formatMinor(data.current_arr_minor, currency),
        hint: 'Annual run-rate',
        trend: computeTrend(
          data.current_arr_minor,
          prevMrrBucket ? prevMrrBucket.mrr_minor * 12 : undefined,
          { format: formatMoneyDelta },
        ),
      },
      {
        label: 'Active subscriptions',
        value: data.active_subscriptions.toLocaleString(),
        hint: `${data.active_customers.toLocaleString()} customers · +${newLast30d.toLocaleString()} new`,
        trend: latestCustomerBucket
          ? {
              direction:
                latestCustomerBucket.new_customers > 0
                  ? 'up'
                  : latestCustomerBucket.new_customers < 0
                    ? 'down'
                    : 'flat',
              tone:
                latestCustomerBucket.new_customers > 0
                  ? 'positive'
                  : 'neutral',
              label: `+${latestCustomerBucket.new_customers} new today`,
            }
          : undefined,
      },
      {
        label: 'Churn (30d)',
        value: data.churned_last_30d.toLocaleString(),
        hint: `ARPU ${formatMinor(data.arpu_minor, currency)}`,
        trend: prevCustomerBucket
          ? computeTrend(
              latestCustomerBucket?.churned_customers ?? 0,
              prevCustomerBucket.churned_customers,
              { inverse: true },
            )
          : latestCustomerBucket
            ? {
                direction:
                  latestCustomerBucket.churned_customers > 0 ? 'up' : 'flat',
                tone:
                  latestCustomerBucket.churned_customers > 0
                    ? 'negative'
                    : 'positive',
                label:
                  latestCustomerBucket.churned_customers > 0
                    ? `${latestCustomerBucket.churned_customers} churned today`
                    : 'no churn today',
              }
            : undefined,
      },
    ]
  }, [data, currency, customerBuckets, mrrBuckets])

  if (isLoading) return <SummarySkeleton />
  if (!data) return null

  return (
    <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
      {cards.map((c) => (
        <Card key={c.label}>
          <CardContent className="p-4">
            <div className="flex items-center justify-between gap-2">
              <p className="text-xs uppercase tracking-wide text-muted-foreground">
                {c.label}
              </p>
              {c.trend && <TrendChip trend={c.trend} />}
            </div>
            <p className="mt-1 text-2xl font-semibold">{c.value}</p>
            <p className="mt-1 text-xs text-muted-foreground">
              {c.trend ? c.trend.label : c.hint}
            </p>
          </CardContent>
        </Card>
      ))}
    </div>
  )
}

function TrendChip({
  trend,
}: {
  trend: NonNullable<SummaryCard['trend']>
}) {
  const Icon =
    trend.direction === 'up'
      ? ArrowUpRight
      : trend.direction === 'down'
        ? ArrowDownRight
        : Minus
  const toneClass =
    trend.tone === 'positive'
      ? 'bg-emerald-500/10 text-emerald-600 dark:text-emerald-400'
      : trend.tone === 'negative'
        ? 'bg-red-500/10 text-red-600 dark:text-red-400'
        : 'bg-muted text-muted-foreground'
  return (
    <span
      className={`inline-flex items-center rounded-full px-1.5 py-0.5 ${toneClass}`}
      aria-label={trend.label}
      title={trend.label}
    >
      <Icon className="h-3 w-3" />
    </span>
  )
}

const mrrChartConfig = {
  mrr: {
    label: 'MRR',
    color: 'var(--chart-1)',
  },
} satisfies ChartConfig

function MrrChart({
  currency,
  isLoading,
  buckets,
}: {
  currency: string
  isLoading: boolean
  buckets: MrrBucketResponse[]
}) {
  const data = useMemo(
    () =>
      buckets.map((b) => ({
        bucket: b.bucket,
        mrr: b.mrr_minor / 100,
      })),
    [buckets],
  )

  return (
    <Card>
      <CardContent className="p-4">
        <div className="mb-3 flex items-center justify-between">
          <h2 className="text-sm font-medium">MRR over time</h2>
        </div>
        {isLoading ? (
          <Skeleton className="h-[250px] w-full" />
        ) : data.length === 0 ? (
          <div className="flex h-[250px] items-center justify-center text-sm text-muted-foreground">
            No revenue data yet.
          </div>
        ) : (
          <ChartContainer config={mrrChartConfig} className="h-[250px] w-full">
            <AreaChart
              accessibilityLayer
              data={data}
              margin={{ left: 12, right: 12, top: 12, bottom: 12 }}
            >
              <CartesianGrid vertical={false} />
              <XAxis
                dataKey="bucket"
                tickLine={false}
                axisLine={false}
                tickMargin={8}
                minTickGap={32}
                tickFormatter={formatBucketShort}
              />
              <YAxis
                tickLine={false}
                axisLine={false}
                tickMargin={8}
                tickFormatter={(v: number) =>
                  formatCurrencyCompact(v, currency)
                }
              />
              <ChartTooltip
                cursor={false}
                content={
                  <ChartTooltipContent
                    labelFormatter={(label) => formatBucketLabel(label as string)}
                    formatter={(value) => [
                      formatMinor((value as number) * 100, currency),
                      ' MRR',
                    ]}
                    indicator="line"
                  />
                }
              />
              <defs>
                <linearGradient id="fillMrr" x1="0" y1="0" x2="0" y2="1">
                  <stop offset="5%" stopColor="var(--color-mrr)" stopOpacity={0.4} />
                  <stop offset="95%" stopColor="var(--color-mrr)" stopOpacity={0.05} />
                </linearGradient>
              </defs>
              <Area
                dataKey="mrr"
                type="monotone"
                stroke="var(--color-mrr)"
                strokeWidth={2}
                fill="url(#fillMrr)"
              />
            </AreaChart>
          </ChartContainer>
        )}
      </CardContent>
    </Card>
  )
}

const customerChartConfig = {
  new: {
    label: 'New',
    color: 'var(--chart-2)',
  },
  churned: {
    label: 'Churned',
    color: 'var(--chart-5)',
  },
} satisfies ChartConfig

function CustomersChart({
  isLoading,
  buckets,
}: {
  isLoading: boolean
  buckets: CustomerMovementResponse[]
}) {
  const data = useMemo(
    () =>
      buckets.map((b) => ({
        bucket: b.bucket,
        new: b.new_customers,
        churned: -b.churned_customers,
      })),
    [buckets],
  )

  return (
    <Card>
      <CardContent className="p-4">
        <div className="mb-3 flex items-center justify-between">
          <h2 className="text-sm font-medium">Customer movement</h2>
          <span className="text-xs text-muted-foreground">
            New vs. churned
          </span>
        </div>
        {isLoading ? (
          <Skeleton className="h-[250px] w-full" />
        ) : data.length === 0 ? (
          <div className="flex h-[250px] items-center justify-center text-sm text-muted-foreground">
            No customer data yet.
          </div>
        ) : (
          <ChartContainer
            config={customerChartConfig}
            className="h-[250px] w-full"
          >
            <BarChart
              accessibilityLayer
              data={data}
              stackOffset="sign"
              margin={{ left: 12, right: 12, top: 12, bottom: 12 }}
            >
              <CartesianGrid vertical={false} />
              <XAxis
                dataKey="bucket"
                tickLine={false}
                axisLine={false}
                tickMargin={8}
                minTickGap={32}
                tickFormatter={formatBucketShort}
              />
              <YAxis
                tickLine={false}
                axisLine={false}
                tickMargin={8}
                tickFormatter={(v: number) => Math.abs(v).toLocaleString()}
              />
              <ChartTooltip
                cursor={false}
                content={
                  <ChartTooltipContent
                    labelFormatter={(label) =>
                      formatBucketLabel(label as string)
                    }
                    formatter={(value, name) => [
                      Math.abs(value as number).toLocaleString(),
                      name === 'new' ? ' New' : ' Churned',
                    ]}
                    indicator="dashed"
                  />
                }
              />
              <Bar
                dataKey="new"
                stackId="movement"
                fill="var(--color-new)"
                radius={[2, 2, 0, 0]}
              />
              <Bar
                dataKey="churned"
                stackId="movement"
                fill="var(--color-churned)"
                radius={[0, 0, 2, 2]}
              />
            </BarChart>
          </ChartContainer>
        )}
      </CardContent>
    </Card>
  )
}

function IntegrationsSection({
  projectId,
  integrations,
}: {
  projectId: number
  integrations: IntegrationResponse[]
}) {
  return (
    <div className="flex flex-col gap-2">
      <h2 className="text-sm font-medium text-muted-foreground">
        Connected providers
      </h2>
      <div className="flex flex-col divide-y rounded-md border">
        {integrations.map((i) => (
          <IntegrationRow key={i.id} projectId={projectId} integration={i} />
        ))}
      </div>
    </div>
  )
}

function IntegrationRow({
  projectId,
  integration,
}: {
  projectId: number
  integration: IntegrationResponse
}) {
  const queryClient = useQueryClient()
  const fullUrl = absoluteWebhookUrl(integration.webhook_path)
  const integrationsKey = revenueListIntegrationsQueryKey({
    path: { project_id: projectId },
  })
  const [updateSecretOpen, setUpdateSecretOpen] = useState(false)
  const [configureOpen, setConfigureOpen] = useState(false)

  const rotate = useMutation({
    ...revenueRotateTokenMutation(),
    onSuccess: () => {
      toast.success('Webhook token rotated. Update the URL in your provider dashboard.')
      queryClient.invalidateQueries({ queryKey: integrationsKey })
    },
    onError: (err: Error) =>
      toast.error(err.message || 'Failed to rotate token'),
  })

  const remove = useMutation({
    ...revenueDeleteIntegrationMutation(),
    onSuccess: () => {
      toast.success('Integration deleted')
      queryClient.invalidateQueries({ queryKey: integrationsKey })
    },
    onError: (err: Error) =>
      toast.error(err.message || 'Failed to delete integration'),
  })

  const handleDelete = () => {
    if (
      confirm(
        'Delete this integration? Historical events are preserved but new webhooks will be rejected.',
      )
    ) {
      remove.mutate({
        path: { project_id: projectId, integration_id: integration.id },
      })
    }
  }

  const handleRotate = () =>
    rotate.mutate({
      path: { project_id: projectId, integration_id: integration.id },
    })

  return (
    <div className="flex items-center gap-3 px-3 py-2.5">
      <div className="flex h-8 w-8 items-center justify-center rounded-md bg-muted">
        <CreditCard className="h-4 w-4" />
      </div>
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <span className="font-medium capitalize">{integration.provider}</span>
          <Badge
            variant={integration.status === 'active' ? 'default' : 'secondary'}
          >
            {integration.status}
          </Badge>
          {integration.last_event_at && (
            <span className="text-xs text-muted-foreground">
              last event{' '}
              <TimeAgo date={new Date(integration.last_event_at)} />
            </span>
          )}
        </div>
        <div className="mt-1 flex items-center gap-1 text-xs text-muted-foreground">
          <code className="truncate">{fullUrl}</code>
          <CopyButton value={fullUrl} />
        </div>
      </div>
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button variant="ghost" size="icon" className="h-8 w-8">
            <MoreVertical className="h-4 w-4" />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end">
          <DropdownMenuItem onClick={handleRotate} disabled={rotate.isPending}>
            <RefreshCw className="mr-2 h-4 w-4" />
            Rotate webhook URL
          </DropdownMenuItem>
          <DropdownMenuItem onClick={() => setUpdateSecretOpen(true)}>
            <KeyRound className="mr-2 h-4 w-4" />
            Update signing secret
          </DropdownMenuItem>
          <DropdownMenuItem onClick={() => setConfigureOpen(true)}>
            <Settings className="mr-2 h-4 w-4" />
            Configure filters
          </DropdownMenuItem>
          <DropdownMenuSeparator />
          <DropdownMenuItem
            onClick={handleDelete}
            className="text-destructive"
          >
            <Trash2 className="mr-2 h-4 w-4" />
            Delete
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
      <UpdateSecretDialog
        projectId={projectId}
        integration={integration}
        open={updateSecretOpen}
        onOpenChange={setUpdateSecretOpen}
      />
      <ConfigureIntegrationDialog
        projectId={projectId}
        integration={integration}
        open={configureOpen}
        onOpenChange={setConfigureOpen}
      />
    </div>
  )
}

function UpdateSecretDialog({
  projectId,
  integration,
  open,
  onOpenChange,
}: {
  projectId: number
  integration: IntegrationResponse
  open: boolean
  onOpenChange: (open: boolean) => void
}) {
  const queryClient = useQueryClient()
  const [signingSecret, setSigningSecret] = useState('')
  const integrationsKey = revenueListIntegrationsQueryKey({
    path: { project_id: projectId },
  })

  useEffect(() => {
    if (open) setSigningSecret('')
  }, [open])

  const update = useMutation({
    ...revenueUpdateSecretMutation(),
    onSuccess: () => {
      toast.success('Signing secret updated.')
      queryClient.invalidateQueries({ queryKey: integrationsKey })
      onOpenChange(false)
    },
    onError: (err: Error) =>
      toast.error(err.message || 'Failed to update signing secret'),
  })

  const canSubmit = signingSecret.trim().length > 0 && !update.isPending

  const handleSubmit = () =>
    update.mutate({
      path: { project_id: projectId, integration_id: integration.id },
      body: { signing_secret: signingSecret.trim() },
    })

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle>Update signing secret</DialogTitle>
          <DialogDescription>
            Paste the new {integration.provider} signing secret. The webhook URL
            stays the same — no changes needed in your provider dashboard.
          </DialogDescription>
        </DialogHeader>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="new-signing-secret">New signing secret</Label>
          <Input
            id="new-signing-secret"
            type="password"
            autoComplete="off"
            placeholder="whsec_..."
            value={signingSecret}
            onChange={(e) => setSigningSecret(e.target.value)}
          />
          <p className="text-xs text-muted-foreground">
            Stored encrypted with AES-256-GCM. Replaces the existing secret on
            save.
          </p>
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            Cancel
          </Button>
          <Button onClick={handleSubmit} disabled={!canSubmit}>
            {update.isPending ? 'Saving…' : 'Save secret'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function ConfigureIntegrationDialog({
  projectId,
  integration,
  open,
  onOpenChange,
}: {
  projectId: number
  integration: IntegrationResponse
  open: boolean
  onOpenChange: (open: boolean) => void
}) {
  const queryClient = useQueryClient()
  const integrationsKey = revenueListIntegrationsQueryKey({
    path: { project_id: projectId },
  })

  // Initial snapshot of allowlist / mode fields pulled from the live config.
  const initial = useMemo(() => deriveConfigState(integration), [integration])
  const [priceList, setPriceList] = useState(initial.priceList)
  const [productList, setProductList] = useState(initial.productList)
  const [variantList, setVariantList] = useState(initial.variantList)
  const [includeUnpriced, setIncludeUnpriced] = useState(initial.includeUnpriced)
  const [meteredMode, setMeteredMode] = useState<MeteredMode>(initial.meteredMode)

  useEffect(() => {
    if (open) {
      const s = deriveConfigState(integration)
      setPriceList(s.priceList)
      setProductList(s.productList)
      setVariantList(s.variantList)
      setIncludeUnpriced(s.includeUnpriced)
      setMeteredMode(s.meteredMode)
    }
  }, [open, integration])

  const update = useMutation({
    ...revenueUpdateConfigMutation(),
    onSuccess: () => {
      toast.success('Filters saved. New events use the updated rules.')
      queryClient.invalidateQueries({ queryKey: integrationsKey })
      onOpenChange(false)
    },
    onError: (err: Error) =>
      toast.error(err.message || 'Failed to save filters'),
  })

  const handleSave = () => {
    const config = buildProviderConfig(integration.provider, {
      priceList,
      productList,
      variantList,
      includeUnpriced,
      meteredMode,
    })
    update.mutate({
      path: { project_id: projectId, integration_id: integration.id },
      body: { config },
    })
  }

  const handleReset = () => {
    update.mutate({
      path: { project_id: projectId, integration_id: integration.id },
      body: { config: null },
    })
  }

  const isStripe = integration.provider === 'stripe'

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle>Configure {integration.provider} filters</DialogTitle>
          <DialogDescription>
            Restrict ingestion to specific SKUs and control how metered/tiered
            subscriptions contribute to MRR. Leave lists empty to accept
            everything.
          </DialogDescription>
        </DialogHeader>

        <div className="flex flex-col gap-4">
          {isStripe ? (
            <>
              <div className="flex flex-col gap-1.5">
                <Label htmlFor="price-allowlist">Price allowlist</Label>
                <Input
                  id="price-allowlist"
                  placeholder="price_1abc, price_2xyz"
                  value={priceList}
                  onChange={(e) => setPriceList(e.target.value)}
                />
                <p className="text-xs text-muted-foreground">
                  Comma-separated Stripe price IDs. Only events tagged with one
                  of these prices are ingested.
                </p>
              </div>
              <div className="flex flex-col gap-1.5">
                <Label htmlFor="product-allowlist">Product allowlist</Label>
                <Input
                  id="product-allowlist"
                  placeholder="prod_1abc, prod_2xyz"
                  value={productList}
                  onChange={(e) => setProductList(e.target.value)}
                />
                <p className="text-xs text-muted-foreground">
                  Comma-separated Stripe product IDs. Combined with price
                  allowlist via OR — if either matches, accept.
                </p>
              </div>
              <label className="flex items-start gap-2 text-sm">
                <input
                  type="checkbox"
                  className="mt-0.5"
                  checked={includeUnpriced}
                  onChange={(e) => setIncludeUnpriced(e.target.checked)}
                />
                <span>
                  Include charges without a price reference
                  <span className="mt-0.5 block text-xs text-muted-foreground">
                    One-off charges (standalone <code>charge.succeeded</code>)
                    don't carry a SKU. Default on.
                  </span>
                </span>
              </label>
              <div className="flex flex-col gap-1.5">
                <Label htmlFor="metered-mode">Metered subscription MRR</Label>
                <Select
                  value={meteredMode}
                  onValueChange={(v) => setMeteredMode(v as MeteredMode)}
                >
                  <SelectTrigger id="metered-mode">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="derive_from_invoices">
                      Derive from invoices (recommended)
                    </SelectItem>
                    <SelectItem value="use_subscription">
                      Use subscription amount
                    </SelectItem>
                    <SelectItem value="ignore">
                      Ignore metered portion
                    </SelectItem>
                  </SelectContent>
                </Select>
                <p className="text-xs text-muted-foreground">
                  For metered/tiered/hybrid subscriptions, MRR is normally 0 at
                  subscription events. "Derive from invoices" backfills MRR from
                  each paid invoice line.
                </p>
              </div>
            </>
          ) : (
            <>
              <div className="flex flex-col gap-1.5">
                <Label htmlFor="variant-allowlist">Variant allowlist</Label>
                <Input
                  id="variant-allowlist"
                  placeholder="variant_1, variant_2"
                  value={variantList}
                  onChange={(e) => setVariantList(e.target.value)}
                />
                <p className="text-xs text-muted-foreground">
                  Comma-separated LemonSqueezy variant IDs.
                </p>
              </div>
              <div className="flex flex-col gap-1.5">
                <Label htmlFor="ls-product-allowlist">Product allowlist</Label>
                <Input
                  id="ls-product-allowlist"
                  placeholder="product_1, product_2"
                  value={productList}
                  onChange={(e) => setProductList(e.target.value)}
                />
                <p className="text-xs text-muted-foreground">
                  Comma-separated LemonSqueezy product IDs.
                </p>
              </div>
            </>
          )}
        </div>

        <DialogFooter className="flex flex-col gap-2 sm:flex-row sm:justify-between">
          <Button
            variant="ghost"
            onClick={handleReset}
            disabled={update.isPending}
            className="sm:mr-auto"
          >
            Reset to defaults
          </Button>
          <div className="flex gap-2">
            <Button variant="outline" onClick={() => onOpenChange(false)}>
              Cancel
            </Button>
            <Button onClick={handleSave} disabled={update.isPending}>
              {update.isPending ? 'Saving…' : 'Save filters'}
            </Button>
          </div>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

interface ConfigState {
  priceList: string
  productList: string
  variantList: string
  includeUnpriced: boolean
  meteredMode: MeteredMode
}

function deriveConfigState(integration: IntegrationResponse): ConfigState {
  const cfg = integration.config ?? null
  if (cfg && cfg.provider === 'stripe') {
    const s = cfg as StripeConfig & { provider: 'stripe' }
    return {
      priceList: (s.price_allowlist ?? []).join(', '),
      productList: (s.product_allowlist ?? []).join(', '),
      variantList: '',
      includeUnpriced: s.include_unpriced_charges ?? true,
      meteredMode: s.metered_mode ?? 'derive_from_invoices',
    }
  }
  if (cfg && cfg.provider === 'lemon_squeezy') {
    const l = cfg as LemonSqueezyConfig & { provider: 'lemon_squeezy' }
    return {
      priceList: '',
      productList: (l.product_allowlist ?? []).join(', '),
      variantList: (l.variant_allowlist ?? []).join(', '),
      includeUnpriced: true,
      meteredMode: 'derive_from_invoices',
    }
  }
  return {
    priceList: '',
    productList: '',
    variantList: '',
    includeUnpriced: true,
    meteredMode: 'derive_from_invoices',
  }
}

function splitList(raw: string): string[] {
  return raw
    .split(',')
    .map((s) => s.trim())
    .filter((s) => s.length > 0)
}

function buildProviderConfig(
  provider: string,
  state: ConfigState,
): ProviderConfig {
  if (provider === 'stripe') {
    return {
      provider: 'stripe',
      price_allowlist: splitList(state.priceList),
      product_allowlist: splitList(state.productList),
      include_unpriced_charges: state.includeUnpriced,
      metered_mode: state.meteredMode,
    }
  }
  return {
    provider: 'lemon_squeezy',
    product_allowlist: splitList(state.productList),
    variant_allowlist: splitList(state.variantList),
  }
}

// ---- ImportDataDialog ----
//
// Hand-written fetch because the endpoint accepts multipart/form-data; the
// generated TanStack Query hooks do not cover FormData bodies cleanly.

interface ImportOutcomeResponse {
  rows_read: number
  inserted: number
  updated: number
  skipped_stale: number
  skipped_invalid: number
  errors: { row: number; reason: string }[]
}

type ImportKind = 'subscriptions' | 'invoices'

async function uploadRevenueCsv(
  projectId: number,
  integrationId: number,
  kind: ImportKind,
  file: File,
): Promise<ImportOutcomeResponse> {
  const form = new FormData()
  form.append('file', file, file.name)
  const res = await fetch(
    `/api/projects/${projectId}/revenue/integrations/${integrationId}/import/${kind}`,
    { method: 'POST', body: form, credentials: 'include' },
  )
  const text = await res.text()
  if (!res.ok) {
    let detail = text
    try {
      const parsed = JSON.parse(text) as { detail?: string; title?: string }
      detail = parsed.detail || parsed.title || text
    } catch {
      // keep raw text
    }
    throw new Error(detail || `Upload failed with status ${res.status}`)
  }
  return JSON.parse(text) as ImportOutcomeResponse
}

function ImportDataDialog({
  projectId,
  integration,
  open,
  onOpenChange,
}: {
  projectId: number
  integration: IntegrationResponse
  open: boolean
  onOpenChange: (open: boolean) => void
}) {
  const queryClient = useQueryClient()
  const [subsFile, setSubsFile] = useState<File | null>(null)
  const [invoicesFile, setInvoicesFile] = useState<File | null>(null)
  const [busy, setBusy] = useState(false)
  const [results, setResults] = useState<
    { kind: ImportKind; outcome: ImportOutcomeResponse }[]
  >([])

  useEffect(() => {
    if (open) {
      setSubsFile(null)
      setInvoicesFile(null)
      setResults([])
      setBusy(false)
    }
  }, [open])

  const canSubmit = !busy && (subsFile !== null || invoicesFile !== null)

  const handleImport = async () => {
    if (!canSubmit) return
    setBusy(true)
    setResults([])
    const collected: { kind: ImportKind; outcome: ImportOutcomeResponse }[] = []
    try {
      if (subsFile) {
        const outcome = await uploadRevenueCsv(
          projectId,
          integration.id,
          'subscriptions',
          subsFile,
        )
        collected.push({ kind: 'subscriptions', outcome })
      }
      if (invoicesFile) {
        const outcome = await uploadRevenueCsv(
          projectId,
          integration.id,
          'invoices',
          invoicesFile,
        )
        collected.push({ kind: 'invoices', outcome })
      }
      setResults(collected)

      const totalInserted = collected.reduce(
        (sum, r) => sum + r.outcome.inserted,
        0,
      )
      const totalUpdated = collected.reduce(
        (sum, r) => sum + r.outcome.updated,
        0,
      )
      toast.success(
        `Imported ${totalInserted} new, updated ${totalUpdated}. MRR refresh in progress.`,
      )
      // Refresh everything that depends on revenue data
      queryClient.invalidateQueries({ queryKey: ['revenue'] })
      queryClient.invalidateQueries({
        queryKey: revenueListIntegrationsQueryKey({
          path: { project_id: projectId },
        }),
      })
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err)
      toast.error(message || 'Import failed')
    } finally {
      setBusy(false)
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle>Import historical revenue</DialogTitle>
          <DialogDescription>
            Upload CSV exports from your provider to backfill MRR and the revenue
            chart. Existing webhook data is never overwritten.
          </DialogDescription>
        </DialogHeader>

        {integration.provider === 'stripe' && (
          <div className="rounded-md border bg-muted/30 p-3 text-xs text-muted-foreground">
            <p className="font-medium text-foreground">How to export from Stripe</p>
            <ol className="mt-1 list-decimal space-y-1 pl-4">
              <li>
                <strong>Subscriptions:</strong> Stripe Dashboard →{' '}
                <em>Billing → Subscriptions</em> → <em>Export</em> (choose{' '}
                <em>All columns</em>).
              </li>
              <li>
                <strong>Invoices (optional):</strong> Stripe Dashboard →{' '}
                <em>Billing → Invoices</em> → filter by <em>Paid</em> →{' '}
                <em>Export</em>.
              </li>
            </ol>
            <p className="mt-2">
              Re-running is safe: rows are matched by Stripe ID and skipped if
              newer webhook data already exists.
            </p>
          </div>
        )}

        <div className="flex flex-col gap-3">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="subs-csv">Subscriptions CSV</Label>
            <Input
              id="subs-csv"
              type="file"
              accept=".csv,text/csv"
              disabled={busy}
              onChange={(e) => setSubsFile(e.target.files?.[0] ?? null)}
            />
            <p className="text-xs text-muted-foreground">
              Drives current MRR, active subscription count, and churn baseline.
            </p>
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="invoices-csv">Invoices CSV (optional)</Label>
            <Input
              id="invoices-csv"
              type="file"
              accept=".csv,text/csv"
              disabled={busy}
              onChange={(e) => setInvoicesFile(e.target.files?.[0] ?? null)}
            />
            <p className="text-xs text-muted-foreground">
              Backfills the historical revenue chart with one-off payments.
            </p>
          </div>
        </div>

        {results.length > 0 && (
          <div className="flex flex-col gap-2 rounded-md border bg-muted/20 p-3 text-xs">
            {results.map((r) => (
              <ImportResultBlock key={r.kind} kind={r.kind} outcome={r.outcome} />
            ))}
          </div>
        )}

        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            {results.length > 0 ? 'Close' : 'Cancel'}
          </Button>
          <Button onClick={handleImport} disabled={!canSubmit}>
            {busy ? 'Importing…' : 'Import'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function ImportResultBlock({
  kind,
  outcome,
}: {
  kind: ImportKind
  outcome: ImportOutcomeResponse
}) {
  const skipped = outcome.skipped_stale + outcome.skipped_invalid
  return (
    <div className="flex flex-col gap-1">
      <div className="flex items-center justify-between">
        <span className="font-medium capitalize text-foreground">{kind}</span>
        <span className="text-muted-foreground">
          {outcome.rows_read} rows · {outcome.inserted} new ·{' '}
          {outcome.updated} updated · {skipped} skipped
          {outcome.errors.length > 0
            ? ` · ${outcome.errors.length} error(s)`
            : ''}
        </span>
      </div>
      {outcome.skipped_stale > 0 && (
        <span className="text-muted-foreground">
          {outcome.skipped_stale} row(s) skipped because newer webhook data
          already exists.
        </span>
      )}
      {outcome.errors.length > 0 && (
        <details>
          <summary className="cursor-pointer text-destructive">
            Show first {Math.min(outcome.errors.length, 5)} error(s)
          </summary>
          <ul className="mt-1 list-disc pl-4 text-destructive">
            {outcome.errors.slice(0, 5).map((e, i) => (
              <li key={i}>
                <span className="font-mono">row {e.row}:</span> {e.reason}
              </li>
            ))}
          </ul>
        </details>
      )}
    </div>
  )
}

function RecentEventsSection({
  isLoading,
  events,
}: {
  isLoading: boolean
  events: RecentEventResponse[]
}) {
  return (
    <div className="flex flex-col gap-2">
      <h2 className="text-sm font-medium text-muted-foreground">
        Recent events
      </h2>
      {isLoading ? (
        <div className="flex flex-col divide-y rounded-md border">
          {Array.from({ length: 4 }).map((_, i) => (
            <div key={i} className="px-3 py-2.5">
              <Skeleton className="mb-1 h-4 w-48" />
              <Skeleton className="h-3 w-32" />
            </div>
          ))}
        </div>
      ) : events.length === 0 ? (
        <div className="rounded-md border p-4 text-sm text-muted-foreground">
          No events yet. Send a test webhook from your provider's dashboard to
          confirm the integration.
        </div>
      ) : (
        <div className="flex flex-col divide-y rounded-md border">
          {events.map((e, idx) => (
            <div
              key={idx}
              className="flex items-center gap-3 px-3 py-2 text-sm"
            >
              <Badge variant="outline" className="font-mono text-[11px]">
                {e.event_type}
              </Badge>
              <span className="min-w-0 flex-1 truncate text-muted-foreground">
                {e.customer_ref ?? '—'}
              </span>
              {e.amount_minor != null && e.currency && (
                <span className="font-medium">
                  {formatMinor(e.amount_minor, e.currency)}
                </span>
              )}
              <span className="text-xs text-muted-foreground">
                <TimeAgo date={new Date(e.occurred_at)} />
              </span>
            </div>
          ))}
        </div>
      )}
    </div>
  )
}

function ConnectProviderDialog({
  projectId,
  open,
  onOpenChange,
  connectedProviders,
}: {
  projectId: number
  open: boolean
  onOpenChange: (open: boolean) => void
  connectedProviders: Set<string>
}) {
  const queryClient = useQueryClient()
  const providersQuery = useQuery({
    ...revenueListProvidersOptions(),
    enabled: open,
  })
  const availableProviders = useMemo(
    () =>
      (providersQuery.data ?? []).filter(
        (p) => !connectedProviders.has(p.name),
      ),
    [providersQuery.data, connectedProviders],
  )
  const integrationsKey = revenueListIntegrationsQueryKey({
    path: { project_id: projectId },
  })
  const [provider, setProvider] = useState<string>('')
  const [signingSecret, setSigningSecret] = useState('')
  const [created, setCreated] = useState<IntegrationResponse | null>(null)

  useEffect(() => {
    if (open) {
      setProvider('')
      setSigningSecret('')
      setCreated(null)
    }
  }, [open])

  useEffect(() => {
    if (!provider && availableProviders.length > 0) {
      setProvider(availableProviders[0].name)
    }
  }, [availableProviders, provider])

  const selected: ProviderDescriptor | undefined = useMemo(
    () => availableProviders.find((p) => p.name === provider),
    [availableProviders, provider],
  )

  const create = useMutation({
    ...revenueCreateIntegrationMutation(),
    onSuccess: (data) => {
      setCreated(data ?? null)
      queryClient.invalidateQueries({ queryKey: integrationsKey })
      toast.success('Integration created. Copy the webhook URL below.')
    },
    onError: (err: Error) =>
      toast.error(err.message || 'Failed to create integration'),
  })

  const canSubmit = provider.length > 0 && signingSecret.trim().length > 0

  const handleCreate = () =>
    create.mutate({
      path: { project_id: projectId },
      body: { provider, signing_secret: signingSecret },
    })

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-lg">
        {created ? (
          <SuccessStep
            integration={created}
            onClose={() => onOpenChange(false)}
          />
        ) : (
          <>
            <DialogHeader>
              <DialogTitle>Connect payment provider</DialogTitle>
              <DialogDescription>
                Temps ingests webhooks from your provider. No API key needed —
                you'll paste a URL into their dashboard.
              </DialogDescription>
            </DialogHeader>
            <div className="flex flex-col gap-4">
              <div className="flex flex-col gap-1.5">
                <Label>Provider</Label>
                <Select value={provider} onValueChange={setProvider}>
                  <SelectTrigger>
                    <SelectValue placeholder="Select a provider" />
                  </SelectTrigger>
                  <SelectContent>
                    {availableProviders.length === 0 ? (
                      <div className="px-3 py-2 text-xs text-muted-foreground">
                        All supported providers are already connected.
                      </div>
                    ) : (
                      availableProviders.map((p) => (
                        <SelectItem key={p.name} value={p.name}>
                          {p.display_name}
                        </SelectItem>
                      ))
                    )}
                  </SelectContent>
                </Select>
              </div>

              {provider === 'stripe' && (
                <StripeInstructions
                  recommendedEvents={selected?.recommended_events ?? []}
                />
              )}

              <div className="flex flex-col gap-1.5">
                <Label htmlFor="signing-secret">Signing secret</Label>
                <Input
                  id="signing-secret"
                  type="password"
                  autoComplete="off"
                  placeholder="whsec_..."
                  value={signingSecret}
                  onChange={(e) => setSigningSecret(e.target.value)}
                />
                <p className="text-xs text-muted-foreground">
                  Stored encrypted with AES-256-GCM. Used to verify webhook
                  signatures.
                </p>
              </div>
            </div>
            <DialogFooter>
              <Button variant="outline" onClick={() => onOpenChange(false)}>
                Cancel
              </Button>
              <Button
                onClick={handleCreate}
                disabled={!canSubmit || create.isPending}
              >
                {create.isPending ? 'Creating…' : 'Create integration'}
              </Button>
            </DialogFooter>
          </>
        )}
      </DialogContent>
    </Dialog>
  )
}

function StripeInstructions({
  recommendedEvents,
}: {
  recommendedEvents: string[]
}) {
  return (
    <div className="rounded-md border bg-muted/30 p-3 text-sm">
      <p className="font-medium">How it works</p>
      <ol className="mt-1 list-decimal space-y-1 pl-5 text-muted-foreground">
        <li>Create this integration. Temps generates a unique webhook URL.</li>
        <li>
          In the Stripe dashboard, go to{' '}
          <strong>Developers → Webhooks → Add endpoint</strong>.
        </li>
        <li>
          Paste the URL we show you next, and copy Stripe's signing secret back
          here.
        </li>
        {recommendedEvents.length > 0 && (
          <li>
            Subscribe to events:{' '}
            <code className="text-[11px]">
              {recommendedEvents.slice(0, 4).join(', ')}
              {recommendedEvents.length > 4 ? '…' : ''}
            </code>
          </li>
        )}
      </ol>
    </div>
  )
}

function SuccessStep({
  integration,
  onClose,
}: {
  integration: IntegrationResponse
  onClose: () => void
}) {
  const url = absoluteWebhookUrl(integration.webhook_path)
  return (
    <>
      <DialogHeader>
        <DialogTitle>Integration created</DialogTitle>
        <DialogDescription>
          Paste this URL into your provider's webhook endpoint configuration.
          Keep this URL secret — anyone with it can post signed events.
        </DialogDescription>
      </DialogHeader>
      <div className="flex flex-col gap-3">
        <div className="flex flex-col gap-1.5">
          <Label>Webhook URL</Label>
          <div className="flex w-full min-w-0 items-center gap-2 rounded-md border bg-muted/40 px-2 py-1.5">
            <code className="min-w-0 flex-1 truncate text-xs">{url}</code>
            <CopyButton value={url} />
          </div>
        </div>
        <p className="text-xs text-muted-foreground">
          Events will start appearing in the Recent events list once your
          provider delivers the first webhook.
        </p>
      </div>
      <DialogFooter>
        <Button onClick={onClose}>Done</Button>
      </DialogFooter>
    </>
  )
}

function formatMinor(amountMinor: number, currency: string): string {
  try {
    return new Intl.NumberFormat(undefined, {
      style: 'currency',
      currency: currency.toUpperCase(),
      maximumFractionDigits: 2,
    }).format(amountMinor / 100)
  } catch {
    return `${(amountMinor / 100).toFixed(2)} ${currency.toUpperCase()}`
  }
}

function formatCurrencyCompact(value: number, currency: string): string {
  try {
    return new Intl.NumberFormat(undefined, {
      style: 'currency',
      currency: currency.toUpperCase(),
      notation: 'compact',
      maximumFractionDigits: 1,
    }).format(value)
  } catch {
    return value.toFixed(0)
  }
}

function formatBucketShort(bucket: string): string {
  const d = new Date(bucket)
  if (Number.isNaN(d.getTime())) return bucket
  return d.toLocaleDateString(undefined, { month: 'short', day: 'numeric' })
}

function formatBucketLabel(bucket: string): string {
  const d = new Date(bucket)
  if (Number.isNaN(d.getTime())) return bucket
  return d.toLocaleDateString(undefined, {
    year: 'numeric',
    month: 'short',
    day: 'numeric',
  })
}

function absoluteWebhookUrl(relativePath: string): string {
  if (typeof window === 'undefined') return relativePath
  return `${window.location.origin}/api${relativePath}`
}
