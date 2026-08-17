import { Fragment, useEffect, useMemo, useState } from 'react'
import { Link, useSearchParams } from 'react-router'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import { useForm, useWatch } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import { z } from 'zod'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { SearchableSelect } from '@/components/ui/searchable-select'
import { Textarea } from '@/components/ui/textarea'
import { Label } from '@/components/ui/label'
import { Badge } from '@/components/ui/badge'
import { Skeleton } from '@/components/ui/skeleton'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { CopyButton } from '@/components/ui/copy-button'
import { CodeTabs, type CodeExample } from '@/components/ui/code-tabs'
import { EmptyState } from '@/components/ui/empty-state'
import {
  ChartConfig,
  ChartContainer,
  ChartTooltip,
  ChartTooltipContent,
} from '@/components/ui/chart'
import {
  Bar,
  BarChart,
  Line,
  LineChart,
  XAxis,
  YAxis,
  CartesianGrid,
} from 'recharts'
import {
  Sheet,
  SheetContent,
  SheetHeader,
  SheetTitle,
} from '@/components/ui/sheet'
import { ScrollArea } from '@/components/ui/scroll-area'
import { Separator } from '@/components/ui/separator'
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from '@/components/ui/tooltip'
import { Progress } from '@/components/ui/progress'
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from '@/components/ui/collapsible'
import {
  Form,
  FormControl,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Checkbox } from '@/components/ui/checkbox'
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
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover'
import {
  Command,
  CommandEmpty,
  CommandInput,
  CommandItem,
  CommandList,
} from '@/components/ui/command'
import {
  Plus,
  Trash2,
  Loader2,
  Power,
  PowerOff,
  Play,
  Activity,
  Zap,
  Clock,
  TrendingUp,
  DollarSign,
  Bot,
  ChevronLeft,
  ChevronRight,
  ChevronDown,
  ChevronsUpDown,
  ListTree,
  ArrowLeft,
  SlidersHorizontal,
  X,
  Wrench,
  MessageSquare,
  AlertTriangle,
  Database,
  Globe,
  Cpu,
  Hash,
  Info,
  RefreshCw,
  ShieldCheck,
  Save,
  Check,
} from 'lucide-react'
import { toast } from 'sonner'
import { AI_CLI_PROVIDER_SHELL } from '@/lib/ai-cli-providers'
import {
  listProviderKeys,
  createProviderKey,
  deleteProviderKey,
  getProviderKey,
  addProviderModel,
  refreshProviderModels,
  deleteProviderModel,
  updateProviderModel,
  updateProviderKey,
  testProviderKeyById,
  saveAiProviderCredential,
  type ProviderKeyResponse,
  type ProviderCatalogDto,
  type ProjectResponse,
  type EnvironmentResponse,
} from '@/api/client'
import {
  getAiProviderStatusOptions,
  getAiProviderStatusQueryKey,
  updateAiProviderPreferenceMutation,
  updateAiSummaryPreferenceMutation,
  listAiProvidersOptions,
  listAiProvidersQueryKey,
  refreshAiProviderStatusMutation,
  getProjectsOptions,
  getEnvironmentsOptions,
  getPricingOptions,
  listGovernanceConfigsOptions,
  listGovernanceConfigsQueryKey,
  upsertGovernanceConfigMutation,
  deleteGovernanceConfigMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { useSettings } from '@/hooks/useSettings'
import { useProjects } from '@/contexts/ProjectsContext'
import {
  AI_PROVIDERS,
  AiProviderIcon,
  aiProviderName,
  aiProviderModels,
  getAiProvider,
} from '@/lib/ai-providers'
import { hostAuthLabel } from '@/lib/ai-cli-auth'
import {
  compareAiModelIdsByRelevance,
  sortAiModelIdsByRelevance,
} from '@/lib/ai-model-ranking'

// Mutation errors here are thrown ProblemDetails response bodies, not Error
// instances -- String(error) on an object stringifies to "[object Object]"
// instead of the API's detail message.
function apiErrorMessage(error: unknown): string {
  if (error && typeof error === 'object' && 'detail' in error) {
    const detail = (error as { detail?: unknown }).detail
    if (typeof detail === 'string' && detail.length > 0) return detail
  }
  return String(error)
}
// ============================================================================
// Usage Analytics types & fetchers
// ============================================================================

interface UsageSummary {
  total_requests: number
  total_input_tokens: number
  total_output_tokens: number
  total_tokens: number
  avg_latency_ms: number
  total_cost_microcents: number
  error_count: number
  streaming_count: number
  byok_count: number
}

interface ProviderUsage {
  provider: string
  request_count: number
  input_tokens: number
  output_tokens: number
  avg_latency_ms: number
  error_count: number
}

interface TimeseriesBucket {
  bucket: string
  request_count: number
  input_tokens: number
  output_tokens: number
  avg_latency_ms: number
}

interface ModelUsage {
  model: string
  provider: string
  request_count: number
  input_tokens: number
  output_tokens: number
  total_tokens: number
  avg_latency_ms: number
}

interface UsageLogEntry {
  id: number
  timestamp: string
  provider: string
  model: string
  input_tokens: number
  output_tokens: number
  latency_ms: number
  estimated_cost_microcents: number
  status: number
  is_streaming: boolean
  is_byok: boolean
}

interface UsageLogPage {
  entries: UsageLogEntry[]
  total: number
}

interface ModelPricing {
  model: string
  provider: string
  input_per_million: number
  output_per_million: number
}

interface PricingResponse {
  models: ModelPricing[]
}

async function fetchJson<T>(url: string): Promise<T> {
  const res = await fetch(url)
  if (!res.ok) throw new Error(`Failed to fetch ${url}: ${res.status}`)
  return res.json()
}

interface SummarySelectOption {
  id: string
  name: string
}

interface SummaryModelOption {
  id: string
  name: string
  thinking_options: SummarySelectOption[]
}

interface SummaryProviderOption {
  id: string
  name: string
  models: SummaryModelOption[]
  default_model_id?: string | null
}

function AiSummaryDefaultsCard({
  providers,
  initialProviderId,
  initialModel,
  initialThinking,
  onSaved,
}: {
  providers: SummaryProviderOption[]
  initialProviderId?: string | null
  initialModel?: string | null
  initialThinking?: string | null
  onSaved: () => void
}) {
  const inherit = '__inherit__'
  const [providerId, setProviderId] = useState(initialProviderId ?? inherit)
  const [model, setModel] = useState(initialModel ?? inherit)
  const [thinking, setThinking] = useState(initialThinking ?? inherit)
  const provider = providers.find((item) => item.id === providerId)
  const effectiveModelId =
    model === inherit ? provider?.default_model_id : model
  const selectedModel = provider?.models.find(
    (item) => item.id === effectiveModelId
  )
  const modelOptions = [
    {
      value: inherit,
      label: provider?.default_model_id
        ? `Use provider/key default (${provider.default_model_id})`
        : 'Use provider/key default',
    },
    ...sortAiModelIdsByRelevance(
      provider?.models.map((item) => item.id) ?? []
    ).map((modelId) => ({
      value: modelId,
      label:
        provider?.models.find((item) => item.id === modelId)?.name ?? modelId,
      keywords: modelId,
    })),
  ]
  const saveMutation = useMutation({
    ...updateAiSummaryPreferenceMutation(),
    onSuccess: () => {
      toast.success('AI summary defaults saved')
      onSaved()
    },
    onError: (error) =>
      toast.error('Could not save AI summary defaults', {
        description: apiErrorMessage(error),
      }),
  })

  return (
    <Card>
      <CardHeader>
        <CardTitle className="text-lg">
          AI summary defaults (all screens)
        </CardTitle>
        <CardDescription>
          Controls only AI summaries across Temps. This is separate from each
          gateway key&apos;s default model; choose “Use provider/key default” to
          inherit that setting.
        </CardDescription>
      </CardHeader>
      <CardContent className="grid gap-4 md:grid-cols-[1fr_1fr_1fr_auto] md:items-end">
        <div className="space-y-2">
          <Label>Summary provider</Label>
          <Select
            value={providerId}
            onValueChange={(value) => {
              setProviderId(value)
              setModel(inherit)
              setThinking(inherit)
            }}
          >
            <SelectTrigger>
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value={inherit}>Use active provider</SelectItem>
              {providers.map((item) => (
                <SelectItem key={item.id} value={item.id}>
                  {item.name}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
        <div className="space-y-2">
          <Label>Summary model</Label>
          <SearchableSelect
            value={model}
            disabled={!provider}
            options={modelOptions}
            placeholder="Use provider/key default"
            searchPlaceholder="Search models by name..."
            emptyText="No matching models."
            searchMode="contains"
            onValueChange={(value) => {
              setModel(value)
              setThinking(inherit)
            }}
          />
        </div>
        <div className="space-y-2">
          <Label>Thinking</Label>
          <Select
            value={thinking}
            disabled={!selectedModel?.thinking_options.length}
            onValueChange={setThinking}
          >
            <SelectTrigger>
              <SelectValue placeholder="Model default" />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value={inherit}>Use model default</SelectItem>
              {selectedModel?.thinking_options.map((item) => (
                <SelectItem key={item.id} value={item.id}>
                  {item.name}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
        <Button
          onClick={() =>
            saveMutation.mutate({
              body: {
                provider_id: providerId === inherit ? null : providerId,
                model: model === inherit ? null : model,
                thinking_level: thinking === inherit ? null : thinking,
              },
            })
          }
          disabled={saveMutation.isPending}
        >
          {saveMutation.isPending && (
            <Loader2 className="mr-2 h-4 w-4 animate-spin" />
          )}
          Save defaults
        </Button>
      </CardContent>
    </Card>
  )
}

function ProviderKeyModelDetail({
  providerKey,
}: {
  providerKey: ProviderKeyResponse
}) {
  const queryClient = useQueryClient()
  const [modelId, setModelId] = useState('')
  const [modelFilter, setModelFilter] = useState('')
  const queryKey = ['providerModelInventory', providerKey.id]
  const detailQuery = useQuery({
    queryKey,
    queryFn: async () => {
      const { data } = await getProviderKey({
        path: { id: providerKey.id },
        throwOnError: true,
      })
      return data
    },
  })
  const refreshMutation = useMutation({
    mutationFn: () =>
      refreshProviderModels({
        path: { id: providerKey.id },
        throwOnError: true,
      }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey })
      toast.success(`${providerKey.display_name} models refreshed`)
    },
    onError: (error) =>
      toast.error('Model refresh failed', {
        description: apiErrorMessage(error),
      }),
  })
  const addMutation = useMutation({
    mutationFn: () =>
      addProviderModel({
        path: { id: providerKey.id },
        body: { model_id: modelId.trim() },
        throwOnError: true,
      }),
    onSuccess: () => {
      setModelId('')
      queryClient.invalidateQueries({ queryKey })
      toast.success('Manual model added')
    },
    onError: (error) =>
      toast.error('Could not add model', {
        description: apiErrorMessage(error),
      }),
  })
  const updateMutation = useMutation({
    mutationFn: ({ id, enabled }: { id: number; enabled: boolean }) =>
      updateProviderModel({
        path: { id: providerKey.id, model_row_id: id },
        body: { is_enabled: enabled },
        throwOnError: true,
      }),
    onSuccess: () => queryClient.invalidateQueries({ queryKey }),
    onError: (error) =>
      toast.error('Could not update model', {
        description: apiErrorMessage(error),
      }),
  })
  const defaultMutation = useMutation({
    mutationFn: (modelId: string) =>
      updateProviderKey({
        path: { id: providerKey.id },
        body: { default_model: modelId },
      }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey })
      queryClient.invalidateQueries({ queryKey: ['providerKeys'] })
      toast.success('Default model updated')
    },
    onError: (error) =>
      toast.error('Could not set default model', {
        description: apiErrorMessage(error),
      }),
  })
  const deleteMutation = useMutation({
    mutationFn: (id: number) =>
      deleteProviderModel({
        path: { id: providerKey.id, model_row_id: id },
        throwOnError: true,
      }),
    onSuccess: () => queryClient.invalidateQueries({ queryKey }),
    onError: (error) =>
      toast.error('Could not delete model', {
        description: apiErrorMessage(error),
      }),
  })

  const models = useMemo(
    () => detailQuery.data?.models ?? [],
    [detailQuery.data?.models]
  )
  const currentDefaultModel = providerKey.default_model
  const visibleModels = useMemo(() => {
    const needle = modelFilter.trim().toLowerCase()
    return [...models]
      .filter((model) => model.model_id.toLowerCase().includes(needle))
      .sort((a, b) => {
        const defaultDifference =
          Number(b.model_id === currentDefaultModel) -
          Number(a.model_id === currentDefaultModel)
        if (defaultDifference) return defaultDifference
        const enabledDifference = Number(b.is_enabled) - Number(a.is_enabled)
        if (enabledDifference) return enabledDifference
        const availableDifference =
          Number(b.is_available) - Number(a.is_available)
        if (availableDifference) return availableDifference
        return compareAiModelIdsByRelevance(a.model_id, b.model_id)
      })
  }, [currentDefaultModel, modelFilter, models])

  if (detailQuery.isLoading) {
    return <Skeleton className="h-24 w-full" />
  }
  if (detailQuery.isError) {
    return (
      <div className="rounded-md border border-destructive/30 p-3 text-sm text-destructive">
        Could not load this key’s model inventory.
      </div>
    )
  }
  const refreshTimes = models
    .map((model) => model.last_seen_at)
    .filter(Boolean)
    .sort()
  const lastRefresh = refreshTimes[refreshTimes.length - 1]

  return (
    <div className="space-y-3 rounded-md border bg-background p-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div>
          <div className="text-sm font-medium">
            Gateway key models and default
          </div>
          <div className="text-xs text-muted-foreground">
            {models.length} model{models.length === 1 ? '' : 's'}
            {lastRefresh
              ? ` · refreshed ${new Date(lastRefresh).toLocaleString()}`
              : ' · bootstrap catalog; refresh to discover live access'}
            {' · '}The key default applies to gateway requests; AI summaries can
            override it above.
          </div>
        </div>
        <Button
          size="sm"
          variant="outline"
          onClick={() => refreshMutation.mutate()}
          disabled={refreshMutation.isPending}
        >
          <RefreshCw
            className={`mr-2 h-3.5 w-3.5 ${refreshMutation.isPending ? 'animate-spin' : ''}`}
          />
          Refresh models
        </Button>
      </div>
      <div className="flex flex-wrap items-center gap-2">
        <Input
          value={modelFilter}
          onChange={(event) => setModelFilter(event.target.value)}
          placeholder="Filter models by name..."
          aria-label="Filter models by name"
          className="max-w-sm"
        />
        <span className="text-xs text-muted-foreground">
          {visibleModels.length} shown · sorted by relevance
        </span>
      </div>
      <div className="max-h-64 overflow-auto rounded-md border">
        {models.length === 0 ? (
          <div className="p-4 text-sm text-muted-foreground">
            No models yet. Refresh from the provider or add a model manually.
          </div>
        ) : visibleModels.length === 0 ? (
          <div className="p-4 text-sm text-muted-foreground">
            No models match “{modelFilter}”.
          </div>
        ) : (
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Model</TableHead>
                <TableHead className="w-28">Source</TableHead>
                <TableHead className="w-28">Availability</TableHead>
                <TableHead className="w-24 text-right">Action</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {visibleModels.map((model) => (
                <TableRow key={model.id}>
                  <TableCell className="py-2">
                    <div className="flex items-center gap-2">
                      <code className="text-xs">{model.model_id}</code>
                      {currentDefaultModel === model.model_id && (
                        <Badge variant="secondary" className="text-[10px]">
                          Default
                        </Badge>
                      )}
                    </div>
                  </TableCell>
                  <TableCell className="py-2">
                    <Badge variant="outline" className="text-[10px]">
                      {model.source}
                    </Badge>
                  </TableCell>
                  <TableCell className="py-2 text-xs">
                    {model.is_available ? 'Available' : 'Not seen'}
                  </TableCell>
                  <TableCell className="py-2 text-right">
                    <div className="flex justify-end gap-1">
                      {model.is_available && model.is_enabled && (
                        <Button
                          variant="ghost"
                          size="sm"
                          disabled={
                            currentDefaultModel === model.model_id ||
                            defaultMutation.isPending
                          }
                          onClick={() => defaultMutation.mutate(model.model_id)}
                        >
                          Use as key default
                        </Button>
                      )}
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={() =>
                          updateMutation.mutate({
                            id: model.id,
                            enabled: !model.is_enabled,
                          })
                        }
                      >
                        {model.is_enabled ? 'Disable' : 'Enable'}
                      </Button>
                      {model.source === 'manual' && (
                        <Button
                          variant="ghost"
                          size="icon"
                          className="h-8 w-8"
                          onClick={() => deleteMutation.mutate(model.id)}
                          title="Delete manual model"
                        >
                          <Trash2 className="h-3.5 w-3.5 text-destructive" />
                        </Button>
                      )}
                    </div>
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        )}
      </div>
      <form
        className="flex gap-2"
        onSubmit={(event) => {
          event.preventDefault()
          if (modelId.trim()) addMutation.mutate()
        }}
      >
        <Input
          value={modelId}
          onChange={(event) => setModelId(event.target.value)}
          placeholder="Add model ID manually"
          className="h-8 text-xs"
        />
        <Button
          type="submit"
          size="sm"
          disabled={!modelId.trim() || addMutation.isPending}
        >
          Add model
        </Button>
      </form>
    </div>
  )
}

function buildUsageUrl(
  path: string,
  params: Record<string, string | undefined>
) {
  const searchParams = new URLSearchParams()
  for (const [key, value] of Object.entries(params)) {
    if (value !== undefined) searchParams.set(key, value)
  }
  const qs = searchParams.toString()
  return `/api/ai/usage/${path}${qs ? `?${qs}` : ''}`
}

function formatTokenCount(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`
  return n.toString()
}

function formatCost(dollars: number): string {
  if (dollars >= 1) return `$${dollars.toFixed(2)}`
  if (dollars >= 0.01) return `$${dollars.toFixed(3)}`
  if (dollars >= 0.001) return `$${dollars.toFixed(4)}`
  if (dollars === 0) return '$0.00'
  return `$${dollars.toFixed(6)}`
}

function findPricing(
  model: string,
  pricingMap: Map<string, ModelPricing>
): ModelPricing | undefined {
  // Exact match first
  const exact = pricingMap.get(model)
  if (exact) return exact

  // Strip common suffixes: -preview, -latest, date stamps like -20250101
  const stripped = model
    .replace(/-preview$/, '')
    .replace(/-latest$/, '')
    .replace(/-\d{6,8}$/, '')
  if (stripped !== model) {
    const match = pricingMap.get(stripped)
    if (match) return match
  }

  // Prefix match: find the longest pricing key that the model starts with
  let best: ModelPricing | undefined
  let bestLen = 0
  for (const [key, pricing] of pricingMap) {
    if (model.startsWith(key) && key.length > bestLen) {
      best = pricing
      bestLen = key.length
    }
  }
  return best
}

function computeCost(
  model: string,
  inputTokens: number,
  outputTokens: number,
  pricingMap: Map<string, ModelPricing>
): number {
  const pricing = findPricing(model, pricingMap)
  if (!pricing) return 0
  return (
    (inputTokens / 1_000_000) * pricing.input_per_million +
    (outputTokens / 1_000_000) * pricing.output_per_million
  )
}

const TIME_RANGES = [
  { label: '24h', hours: 24 },
  { label: '7d', hours: 168 },
  { label: '30d', hours: 720 },
] as const

type TimeRange = (typeof TIME_RANGES)[number]

function useTimeRangeSelection(): [
  { range: TimeRange; endMs: number },
  (range: TimeRange) => void,
] {
  const [selection, setSelection] = useState<{
    range: TimeRange
    endMs: number
  }>(() => ({ range: TIME_RANGES[0], endMs: Date.now() }))
  const selectRange = (range: TimeRange) =>
    setSelection({ range, endMs: Date.now() })
  return [selection, selectRange]
}

// Back-compat local aliases — the shared registry now lives in
// `@/lib/ai-providers` and is reused by AiQuickstart, provider detail
// pages, and the sandbox UI.
const SUPPORTED_PROVIDERS = AI_PROVIDERS
const providerName = aiProviderName
const providerModels = aiProviderModels

// ============================================================================
// Usage Analytics Component
// ============================================================================

const requestsChartConfig = {
  request_count: { label: 'Requests', color: 'var(--chart-1)' },
} satisfies ChartConfig

const tokensChartConfig = {
  input_tokens: { label: 'Input', color: 'var(--chart-1)' },
  output_tokens: { label: 'Output', color: 'var(--chart-2)' },
} satisfies ChartConfig

const latencyChartConfig = {
  avg_latency_ms: { label: 'Avg Latency (ms)', color: 'var(--chart-3)' },
} satisfies ChartConfig

const providerChartConfig = {
  request_count: { label: 'Requests', color: 'var(--chart-1)' },
} satisfies ChartConfig

function LogCostCell({
  log,
  pricingMap,
}: {
  log: UsageLogEntry
  pricingMap: Map<string, ModelPricing>
}) {
  const cost = computeCost(
    log.model,
    log.input_tokens,
    log.output_tokens,
    pricingMap
  )
  return <>{cost > 0 ? formatCost(cost) : '—'}</>
}

export function UsageAnalytics() {
  const [{ range: timeRange, endMs }, selectTimeRange] = useTimeRangeSelection()

  const timeParams = useMemo(() => {
    const to = new Date(endMs).toISOString()
    const from = new Date(endMs - timeRange.hours * 3600_000).toISOString()
    const bucket = timeRange.hours <= 24 ? 'hour' : 'day'
    return { from, to, bucket }
  }, [endMs, timeRange])

  const { data: summary, isLoading: summaryLoading } = useQuery({
    queryKey: ['aiUsage', 'summary', timeParams.from, timeParams.to],
    queryFn: () =>
      fetchJson<UsageSummary>(
        buildUsageUrl('summary', { from: timeParams.from, to: timeParams.to })
      ),
  })

  const { data: timeseries, isLoading: timeseriesLoading } = useQuery({
    queryKey: [
      'aiUsage',
      'timeseries',
      timeParams.from,
      timeParams.to,
      timeParams.bucket,
    ],
    queryFn: () =>
      fetchJson<TimeseriesBucket[]>(
        buildUsageUrl('timeseries', {
          from: timeParams.from,
          to: timeParams.to,
          bucket: timeParams.bucket,
        })
      ),
  })

  const { data: byProvider } = useQuery({
    queryKey: ['aiUsage', 'by-provider', timeParams.from, timeParams.to],
    queryFn: () =>
      fetchJson<ProviderUsage[]>(
        buildUsageUrl('by-provider', {
          from: timeParams.from,
          to: timeParams.to,
        })
      ),
  })

  const { data: topModels } = useQuery({
    queryKey: ['aiUsage', 'top-models', timeParams.from, timeParams.to],
    queryFn: () =>
      fetchJson<ModelUsage[]>(
        buildUsageUrl('top-models', {
          from: timeParams.from,
          to: timeParams.to,
          limit: '10',
        })
      ),
  })

  // Recent Requests: pagination + filters. `pageSize` is user-configurable up
  // to the backend max of 50. Filters reset the page back to 0 on change.
  const [recentPage, setRecentPage] = useState(0)
  const [recentPageSize, setRecentPageSize] = useState(20)
  const [recentProvider, setRecentProvider] = useState('all')
  const [recentStatus, setRecentStatus] = useState('all')
  const [recentCostOp, setRecentCostOp] = useState<'gte' | 'gt' | 'lte' | 'lt'>(
    'gte'
  )
  const [recentCostInput, setRecentCostInput] = useState('')
  const [recentTokensOp, setRecentTokensOp] = useState<
    'gte' | 'gt' | 'lte' | 'lt'
  >('gte')
  const [recentTokensInput, setRecentTokensInput] = useState('')
  // The filter row is collapsed by default and only revealed on demand.
  const [recentFiltersOpen, setRecentFiltersOpen] = useState(false)

  // Cost is entered by the user in dollars; the API expects microcents.
  const recentCostMicrocents = useMemo(() => {
    const dollars = parseFloat(recentCostInput)
    if (!Number.isFinite(dollars) || dollars < 0) return undefined
    return Math.round(dollars * 1_000_000 * 100)
  }, [recentCostInput])

  // Tokens are entered as a plain integer count.
  const recentTokensValue = useMemo(() => {
    const n = parseInt(recentTokensInput, 10)
    if (!Number.isFinite(n) || n < 0) return undefined
    return n
  }, [recentTokensInput])

  const recentFilterParams = useMemo(() => {
    const params: Record<string, string | undefined> = {}
    if (recentProvider !== 'all') params.provider = recentProvider
    if (recentStatus !== 'all') params.status = recentStatus
    if (recentCostMicrocents !== undefined) {
      params[`cost_${recentCostOp}`] = String(recentCostMicrocents)
    }
    if (recentTokensValue !== undefined) {
      params[`tokens_${recentTokensOp}`] = String(recentTokensValue)
    }
    return params
  }, [
    recentProvider,
    recentStatus,
    recentCostOp,
    recentCostMicrocents,
    recentTokensOp,
    recentTokensValue,
  ])

  const recentActiveFilterCount = Object.keys(recentFilterParams).length

  const { data: recentLogsPage, isPlaceholderData: recentIsPlaceholder } =
    useQuery({
      queryKey: [
        'aiUsage',
        'recent',
        recentPage,
        recentPageSize,
        recentFilterParams,
      ],
      queryFn: () =>
        fetchJson<UsageLogPage>(
          buildUsageUrl('recent', {
            limit: String(recentPageSize),
            offset: String(recentPage * recentPageSize),
            ...recentFilterParams,
          })
        ),
      placeholderData: (prev) => prev,
    })

  const recentLogs = recentLogsPage?.entries
  const recentTotal = recentLogsPage?.total ?? 0
  const recentTotalPages = Math.max(1, Math.ceil(recentTotal / recentPageSize))
  const recentHasFilters = recentActiveFilterCount > 0

  // Reset to the first page whenever filters or page size change, so the user
  // never lands on an out-of-range page after narrowing the result set.
  const recentResetKey = `${recentPageSize}|${JSON.stringify(recentFilterParams)}`
  const [recentLastResetKey, setRecentLastResetKey] = useState(recentResetKey)
  if (recentResetKey !== recentLastResetKey) {
    setRecentLastResetKey(recentResetKey)
    setRecentPage(0)
  }

  const { data: pricingData } = useQuery({
    queryKey: ['aiPricing'],
    queryFn: () => fetchJson<PricingResponse>('/api/ai/pricing'),
    staleTime: 60 * 60 * 1000,
  })

  const pricingMap = useMemo(() => {
    const map = new Map<string, ModelPricing>()
    for (const m of pricingData?.models ?? []) {
      map.set(m.model, m)
    }
    return map
  }, [pricingData])

  const totalEstimatedCost = useMemo(() => {
    if (!topModels || pricingMap.size === 0) return 0
    return topModels.reduce(
      (sum, m) =>
        sum + computeCost(m.model, m.input_tokens, m.output_tokens, pricingMap),
      0
    )
  }, [topModels, pricingMap])

  const chartTimeseries = useMemo(
    () =>
      (timeseries ?? []).map((b) => ({
        ...b,
        label:
          timeParams.bucket === 'hour'
            ? new Date(b.bucket).toLocaleTimeString([], {
                hour: '2-digit',
                minute: '2-digit',
              })
            : new Date(b.bucket).toLocaleDateString([], {
                month: 'short',
                day: 'numeric',
              }),
      })),
    [timeseries, timeParams.bucket]
  )

  return (
    <div className="space-y-4">
      {/* Time range selector */}
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <h3 className="text-lg font-semibold">Usage Analytics</h3>
        <div className="flex gap-1">
          {TIME_RANGES.map((range) => (
            <Button
              key={range.label}
              variant={timeRange.label === range.label ? 'default' : 'outline'}
              size="sm"
              onClick={() => selectTimeRange(range)}
            >
              {range.label}
            </Button>
          ))}
        </div>
      </div>

      {/* Summary stat cards */}
      <div className="grid gap-3 sm:gap-4 grid-cols-2 md:grid-cols-5">
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm font-medium flex items-center gap-2">
              <Activity className="h-4 w-4 text-muted-foreground" />
              Requests
            </CardTitle>
          </CardHeader>
          <CardContent>
            {summaryLoading ? (
              <Skeleton className="h-7 w-16" />
            ) : (
              <div className="text-2xl font-bold">
                {formatTokenCount(summary?.total_requests ?? 0)}
              </div>
            )}
          </CardContent>
        </Card>
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm font-medium flex items-center gap-2">
              <Zap className="h-4 w-4 text-muted-foreground" />
              Total Tokens
            </CardTitle>
          </CardHeader>
          <CardContent>
            {summaryLoading ? (
              <Skeleton className="h-7 w-16" />
            ) : (
              <div className="text-2xl font-bold">
                {formatTokenCount(summary?.total_tokens ?? 0)}
              </div>
            )}
          </CardContent>
        </Card>
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm font-medium flex items-center gap-2">
              <Clock className="h-4 w-4 text-muted-foreground" />
              Avg Latency
            </CardTitle>
          </CardHeader>
          <CardContent>
            {summaryLoading ? (
              <Skeleton className="h-7 w-16" />
            ) : (
              <div className="text-2xl font-bold">
                {Math.round(summary?.avg_latency_ms ?? 0)}ms
              </div>
            )}
          </CardContent>
        </Card>
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm font-medium flex items-center gap-2">
              <TrendingUp className="h-4 w-4 text-muted-foreground" />
              Error Rate
            </CardTitle>
          </CardHeader>
          <CardContent>
            {summaryLoading ? (
              <Skeleton className="h-7 w-16" />
            ) : (
              <div className="text-2xl font-bold">
                {summary && summary.total_requests > 0
                  ? `${((summary.error_count / summary.total_requests) * 100).toFixed(1)}%`
                  : '0%'}
              </div>
            )}
          </CardContent>
        </Card>
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm font-medium flex items-center gap-2">
              <DollarSign className="h-4 w-4 text-muted-foreground" />
              Est. Cost
            </CardTitle>
          </CardHeader>
          <CardContent>
            {summaryLoading ? (
              <Skeleton className="h-7 w-16" />
            ) : (
              <div className="text-2xl font-bold">
                {formatCost(totalEstimatedCost)}
              </div>
            )}
          </CardContent>
        </Card>
      </div>

      {/* Requests over time chart */}
      <Card>
        <CardHeader>
          <CardTitle className="text-base">Requests Over Time</CardTitle>
        </CardHeader>
        <CardContent>
          {timeseriesLoading ? (
            <Skeleton className="h-[200px] w-full" />
          ) : chartTimeseries.length === 0 ? (
            <EmptyState
              icon={Activity}
              title="No usage data"
              description="Make some requests through the AI Gateway to see analytics here."
            />
          ) : (
            <ChartContainer
              config={requestsChartConfig}
              className="h-[200px] w-full"
            >
              <BarChart data={chartTimeseries} accessibilityLayer>
                <CartesianGrid vertical={false} />
                <XAxis
                  dataKey="label"
                  tickLine={false}
                  axisLine={false}
                  fontSize={12}
                />
                <YAxis tickLine={false} axisLine={false} fontSize={12} />
                <ChartTooltip content={<ChartTooltipContent />} />
                <Bar
                  dataKey="request_count"
                  fill="var(--color-request_count)"
                  radius={[4, 4, 0, 0]}
                />
              </BarChart>
            </ChartContainer>
          )}
        </CardContent>
      </Card>

      {/* Token usage + Latency charts side by side */}
      <div className="grid gap-4 md:grid-cols-2">
        <Card>
          <CardHeader>
            <CardTitle className="text-base">Token Usage</CardTitle>
          </CardHeader>
          <CardContent>
            {timeseriesLoading ? (
              <Skeleton className="h-[200px] w-full" />
            ) : chartTimeseries.length === 0 ? (
              <div className="h-[200px] flex items-center justify-center text-sm text-muted-foreground">
                No data
              </div>
            ) : (
              <ChartContainer
                config={tokensChartConfig}
                className="h-[200px] w-full"
              >
                <LineChart data={chartTimeseries} accessibilityLayer>
                  <CartesianGrid vertical={false} />
                  <XAxis
                    dataKey="label"
                    tickLine={false}
                    axisLine={false}
                    fontSize={12}
                  />
                  <YAxis
                    tickLine={false}
                    axisLine={false}
                    fontSize={12}
                    tickFormatter={formatTokenCount}
                  />
                  <ChartTooltip content={<ChartTooltipContent />} />
                  <Line
                    type="monotone"
                    dataKey="input_tokens"
                    stroke="var(--color-input_tokens)"
                    strokeWidth={2}
                    dot={false}
                  />
                  <Line
                    type="monotone"
                    dataKey="output_tokens"
                    stroke="var(--color-output_tokens)"
                    strokeWidth={2}
                    dot={false}
                  />
                </LineChart>
              </ChartContainer>
            )}
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle className="text-base">Avg Latency</CardTitle>
          </CardHeader>
          <CardContent>
            {timeseriesLoading ? (
              <Skeleton className="h-[200px] w-full" />
            ) : chartTimeseries.length === 0 ? (
              <div className="h-[200px] flex items-center justify-center text-sm text-muted-foreground">
                No data
              </div>
            ) : (
              <ChartContainer
                config={latencyChartConfig}
                className="h-[200px] w-full"
              >
                <LineChart data={chartTimeseries} accessibilityLayer>
                  <CartesianGrid vertical={false} />
                  <XAxis
                    dataKey="label"
                    tickLine={false}
                    axisLine={false}
                    fontSize={12}
                  />
                  <YAxis
                    tickLine={false}
                    axisLine={false}
                    fontSize={12}
                    unit="ms"
                  />
                  <ChartTooltip content={<ChartTooltipContent />} />
                  <Line
                    type="monotone"
                    dataKey="avg_latency_ms"
                    stroke="var(--color-avg_latency_ms)"
                    strokeWidth={2}
                    dot={false}
                  />
                </LineChart>
              </ChartContainer>
            )}
          </CardContent>
        </Card>
      </div>

      {/* Per-provider breakdown + Top models */}
      <div className="grid gap-4 md:grid-cols-2">
        <Card>
          <CardHeader>
            <CardTitle className="text-base">By Provider</CardTitle>
          </CardHeader>
          <CardContent>
            {!byProvider || byProvider.length === 0 ? (
              <div className="h-[200px] flex items-center justify-center text-sm text-muted-foreground">
                No data
              </div>
            ) : (
              <ChartContainer
                config={providerChartConfig}
                className="h-[200px] w-full"
              >
                <BarChart
                  data={byProvider.map((p) => ({
                    ...p,
                    name: providerName(p.provider),
                  }))}
                  layout="vertical"
                  accessibilityLayer
                >
                  <CartesianGrid horizontal={false} />
                  <XAxis
                    type="number"
                    tickLine={false}
                    axisLine={false}
                    fontSize={12}
                  />
                  <YAxis
                    dataKey="name"
                    type="category"
                    tickLine={false}
                    axisLine={false}
                    fontSize={12}
                    width={80}
                  />
                  <ChartTooltip content={<ChartTooltipContent />} />
                  <Bar
                    dataKey="request_count"
                    fill="var(--color-request_count)"
                    radius={[0, 4, 4, 0]}
                  />
                </BarChart>
              </ChartContainer>
            )}
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle className="text-base">Top Models</CardTitle>
          </CardHeader>
          <CardContent>
            {!topModels || topModels.length === 0 ? (
              <div className="h-[200px] flex items-center justify-center text-sm text-muted-foreground">
                No data
              </div>
            ) : (
              <div className="space-y-3">
                {topModels.map((m) => {
                  const cost = computeCost(
                    m.model,
                    m.input_tokens,
                    m.output_tokens,
                    pricingMap
                  )
                  return (
                    <div
                      key={m.model}
                      className="flex flex-col gap-1 sm:flex-row sm:items-center sm:justify-between text-sm"
                    >
                      <div className="flex items-center gap-2 min-w-0">
                        <span className="font-mono truncate">{m.model}</span>
                        <Badge
                          variant="outline"
                          className="text-[10px] shrink-0"
                        >
                          {providerName(m.provider)}
                        </Badge>
                      </div>
                      <div className="flex items-center gap-3 sm:gap-4 shrink-0 text-muted-foreground text-xs sm:text-sm">
                        <span>{formatTokenCount(m.request_count)} req</span>
                        <span>{formatTokenCount(m.total_tokens)} tok</span>
                        {cost > 0 && (
                          <span className="text-orange-500 font-medium">
                            {formatCost(cost)}
                          </span>
                        )}
                      </div>
                    </div>
                  )
                })}
              </div>
            )}
          </CardContent>
        </Card>
      </div>

      {/* Recent requests */}
      <Card>
        <CardHeader className="gap-3">
          <div className="flex items-center justify-between gap-2">
            <CardTitle className="text-base">Recent Requests</CardTitle>
            <Button
              type="button"
              variant={recentFiltersOpen ? 'secondary' : 'outline'}
              size="sm"
              onClick={() => setRecentFiltersOpen((open) => !open)}
            >
              <SlidersHorizontal className="h-4 w-4" />
              <span className="hidden sm:inline">Filters</span>
              {recentActiveFilterCount > 0 && (
                <Badge
                  variant="secondary"
                  className="ml-1 h-5 min-w-5 justify-center px-1 text-xs tabular-nums"
                >
                  {recentActiveFilterCount}
                </Badge>
              )}
            </Button>
          </div>
          {/* Filter row — hidden until the user opens it, or while a filter is active */}
          {(recentFiltersOpen || recentHasFilters) && (
            <div className="flex flex-col gap-2 rounded-lg border bg-muted/30 p-3 sm:flex-row sm:flex-wrap sm:items-center">
              <Select value={recentProvider} onValueChange={setRecentProvider}>
                <SelectTrigger className="w-full sm:w-[160px]">
                  <SelectValue placeholder="Provider" />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="all">All providers</SelectItem>
                  {/* Source the full supported-provider set, not the time-windowed
                      analytics query — the recent list isn't bound to that window. */}
                  {AI_PROVIDERS.map((p) => (
                    <SelectItem key={p.id} value={p.id}>
                      {p.name}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              <Select value={recentStatus} onValueChange={setRecentStatus}>
                <SelectTrigger className="w-full sm:w-[150px]">
                  <SelectValue placeholder="Status" />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="all">All statuses</SelectItem>
                  <SelectItem value="200">200 OK</SelectItem>
                  <SelectItem value="400">400 Bad Request</SelectItem>
                  <SelectItem value="401">401 Unauthorized</SelectItem>
                  <SelectItem value="403">403 Forbidden</SelectItem>
                  <SelectItem value="404">404 Not Found</SelectItem>
                  <SelectItem value="429">429 Rate Limited</SelectItem>
                  <SelectItem value="500">500 Server Error</SelectItem>
                </SelectContent>
              </Select>
              <div className="flex items-center gap-2">
                <Select
                  value={recentCostOp}
                  onValueChange={(v) =>
                    setRecentCostOp(v as typeof recentCostOp)
                  }
                >
                  <SelectTrigger className="w-[92px]">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="gte">Cost ≥</SelectItem>
                    <SelectItem value="gt">Cost &gt;</SelectItem>
                    <SelectItem value="lte">Cost ≤</SelectItem>
                    <SelectItem value="lt">Cost &lt;</SelectItem>
                  </SelectContent>
                </Select>
                <div className="relative w-full sm:w-[110px]">
                  <span className="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 text-sm text-muted-foreground">
                    $
                  </span>
                  <Input
                    type="number"
                    name="recent-cost-filter"
                    aria-label="Filter by cost in dollars"
                    min="0"
                    step="0.01"
                    inputMode="decimal"
                    placeholder="0.00"
                    value={recentCostInput}
                    onChange={(e) => setRecentCostInput(e.target.value)}
                    className="pl-6"
                  />
                </div>
              </div>
              <div className="flex items-center gap-2">
                <Select
                  value={recentTokensOp}
                  onValueChange={(v) =>
                    setRecentTokensOp(v as typeof recentTokensOp)
                  }
                >
                  <SelectTrigger className="w-[104px]">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="gte">Tokens ≥</SelectItem>
                    <SelectItem value="gt">Tokens &gt;</SelectItem>
                    <SelectItem value="lte">Tokens ≤</SelectItem>
                    <SelectItem value="lt">Tokens &lt;</SelectItem>
                  </SelectContent>
                </Select>
                <Input
                  type="number"
                  name="recent-tokens-filter"
                  aria-label="Filter by total token count"
                  min="0"
                  step="100"
                  inputMode="numeric"
                  placeholder="0"
                  value={recentTokensInput}
                  onChange={(e) => setRecentTokensInput(e.target.value)}
                  className="w-full sm:w-[100px]"
                />
              </div>
              {recentHasFilters && (
                <Button
                  type="button"
                  variant="ghost"
                  size="sm"
                  onClick={() => {
                    setRecentProvider('all')
                    setRecentStatus('all')
                    setRecentCostInput('')
                    setRecentTokensInput('')
                  }}
                >
                  <X className="h-4 w-4" />
                  Clear
                </Button>
              )}
            </div>
          )}
        </CardHeader>
        <CardContent>
          {!recentLogs || recentLogs.length === 0 ? (
            <div className="py-8 text-center text-sm text-muted-foreground">
              {recentHasFilters
                ? 'No requests match these filters'
                : 'No recent requests'}
            </div>
          ) : (
            <div
              className={`overflow-x-auto transition-opacity ${
                recentIsPlaceholder ? 'opacity-60' : ''
              }`}
            >
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>Time</TableHead>
                    <TableHead>Provider</TableHead>
                    <TableHead>Model</TableHead>
                    <TableHead className="hidden md:table-cell">
                      Tokens
                    </TableHead>
                    <TableHead className="hidden md:table-cell">
                      Latency
                    </TableHead>
                    <TableHead className="hidden md:table-cell">Cost</TableHead>
                    <TableHead>Status</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {recentLogs.map((log) => (
                    <TableRow key={log.id}>
                      <TableCell className="text-muted-foreground text-xs whitespace-nowrap">
                        {new Date(log.timestamp).toLocaleString([], {
                          month: 'short',
                          day: 'numeric',
                          hour: '2-digit',
                          minute: '2-digit',
                        })}
                      </TableCell>
                      <TableCell className="font-medium">
                        {providerName(log.provider)}
                      </TableCell>
                      <TableCell className="font-mono text-xs">
                        {log.model}
                      </TableCell>
                      <TableCell className="hidden md:table-cell text-muted-foreground">
                        {formatTokenCount(log.input_tokens + log.output_tokens)}
                      </TableCell>
                      <TableCell className="hidden md:table-cell text-muted-foreground">
                        {log.latency_ms}ms
                      </TableCell>
                      <TableCell className="hidden md:table-cell text-muted-foreground">
                        <LogCostCell log={log} pricingMap={pricingMap} />
                      </TableCell>
                      <TableCell>
                        <div className="flex items-center gap-1.5">
                          <Badge
                            variant={
                              log.status < 400 ? 'default' : 'destructive'
                            }
                            className={
                              log.status < 400
                                ? 'bg-green-500/15 text-green-500 hover:bg-green-500/25'
                                : ''
                            }
                          >
                            {log.status}
                          </Badge>
                          {log.is_streaming && (
                            <Badge variant="outline" className="text-[10px]">
                              stream
                            </Badge>
                          )}
                          {log.is_byok && (
                            <Badge variant="outline" className="text-[10px]">
                              BYOK
                            </Badge>
                          )}
                        </div>
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>
          )}
          {recentTotal > 0 && (
            <div className="mt-4 flex flex-col gap-2 border-t pt-3 sm:flex-row sm:items-center sm:justify-between">
              <div className="flex items-center gap-3">
                <p className="text-xs text-muted-foreground">
                  <span className="hidden sm:inline">
                    Showing {recentPage * recentPageSize + 1}–
                    {Math.min((recentPage + 1) * recentPageSize, recentTotal)}{' '}
                    of {recentTotal.toLocaleString()}
                  </span>
                  <span className="sm:hidden">
                    {recentPage + 1} / {recentTotalPages}
                  </span>
                </p>
                <Select
                  value={String(recentPageSize)}
                  onValueChange={(v) => setRecentPageSize(Number(v))}
                >
                  <SelectTrigger className="h-8 w-[110px] text-xs">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {[10, 20, 30, 50].map((size) => (
                      <SelectItem key={size} value={String(size)}>
                        {size} / page
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
              <div className="flex items-center gap-2">
                <Button
                  variant="outline"
                  size="sm"
                  disabled={recentPage === 0}
                  onClick={() => setRecentPage((p) => Math.max(0, p - 1))}
                >
                  <ChevronLeft className="h-4 w-4" />
                  <span className="hidden sm:inline">Previous</span>
                </Button>
                <Button
                  variant="outline"
                  size="sm"
                  disabled={recentPage + 1 >= recentTotalPages}
                  onClick={() => setRecentPage((p) => p + 1)}
                >
                  <span className="hidden sm:inline">Next</span>
                  <ChevronRight className="h-4 w-4" />
                </Button>
              </div>
            </div>
          )}
        </CardContent>
      </Card>
    </div>
  )
}

// ============================================================================
// GenAI Agent Activity Types & Component
// ============================================================================

interface GenAiTraceSummary {
  trace_id: string
  root_span_name: string
  service_name: string
  gen_ai_system: string | null
  gen_ai_model: string | null
  gen_ai_operation: string | null
  start_time: string
  duration_ms: number
  span_count: number
  error_count: number
  total_input_tokens: number | null
  total_output_tokens: number | null
  total_cache_creation_input_tokens: number | null
  total_cache_read_input_tokens: number | null
}

interface GenAiSpanDetail {
  span_id: string
  parent_span_id: string | null
  name: string
  kind: string
  start_time: string
  duration_ms: number
  status_code: string

  // Core identification
  gen_ai_system: string | null
  gen_ai_operation: string | null

  // Model
  gen_ai_model: string | null
  gen_ai_response_model: string | null

  // Request parameters
  request_temperature: number | null
  request_max_tokens: number | null
  request_top_p: number | null
  request_top_k: number | null
  request_frequency_penalty: number | null
  request_presence_penalty: number | null
  request_stop_sequences: string[] | null
  request_seed: number | null
  request_choice_count: number | null

  // Response
  response_id: string | null
  response_finish_reasons: string[] | null
  output_type: string | null

  // Token usage
  input_tokens: number | null
  output_tokens: number | null
  cache_creation_input_tokens: number | null
  cache_read_input_tokens: number | null

  // Conversation / Error / Server
  conversation_id: string | null
  error_type: string | null
  server_address: string | null
  server_port: number | null

  // Agent
  agent_id: string | null
  agent_name: string | null
  agent_description: string | null
  agent_version: string | null

  // Tool
  tool_name: string | null
  tool_call_id: string | null
  tool_type: string | null
  tool_description: string | null

  // Embeddings
  embeddings_dimension_count: number | null
  request_encoding_formats: string[] | null

  // Retrieval
  data_source_id: string | null

  // Provider-specific
  openai_api_type: string | null
  openai_request_service_tier: string | null
  openai_response_service_tier: string | null
  openai_system_fingerprint: string | null
  aws_bedrock_guardrail_id: string | null
  aws_bedrock_knowledge_base_id: string | null
  azure_resource_provider_namespace: string | null

  // Opt-in content
  input_messages: string | null
  output_messages: string | null
  system_instructions: string | null
  tool_definitions: string | null
  tool_call_arguments: string | null
  tool_call_result: string | null
  retrieval_query_text: string | null
  retrieval_documents: string | null

  attributes: Record<string, string>
}

interface GenAiEvent {
  span_id: string
  trace_id: string
  event_name: string
  timestamp: string
  attributes: Record<string, string>
}

interface GenAiTraceSummariesResponse {
  data: GenAiTraceSummary[]
  total: number
}

interface GenAiTraceDetailResponse {
  trace_id: string
  spans: GenAiSpanDetail[]
  span_count: number
  events: GenAiEvent[]
  event_count: number
}

function buildOtelUrl(
  path: string,
  params: Record<string, string | number | undefined>
) {
  const searchParams = new URLSearchParams()
  for (const [key, value] of Object.entries(params)) {
    if (value !== undefined) searchParams.set(key, String(value))
  }
  const qs = searchParams.toString()
  return `/api/otel/${path}${qs ? `?${qs}` : ''}`
}

// ── Span tree helpers ───────────────────────────────────────────────

interface SpanTreeNode {
  span: GenAiSpanDetail
  children: SpanTreeNode[]
}

function buildSpanTree(spans: GenAiSpanDetail[]): SpanTreeNode[] {
  const byId = new Map<string, SpanTreeNode>()
  for (const span of spans) {
    byId.set(span.span_id, { span, children: [] })
  }
  const roots: SpanTreeNode[] = []
  for (const node of byId.values()) {
    const parentId = node.span.parent_span_id
    if (parentId && byId.has(parentId)) {
      byId.get(parentId)!.children.push(node)
    } else {
      roots.push(node)
    }
  }
  return roots
}

function spanIcon(span: GenAiSpanDetail, className: string) {
  if (span.gen_ai_operation === 'execute_tool' || span.tool_name)
    return <Wrench className={className} />
  if (
    span.agent_name ||
    span.gen_ai_operation === 'invoke_agent' ||
    span.gen_ai_operation === 'create_agent'
  )
    return <Bot className={className} />
  if (span.gen_ai_operation === 'embeddings')
    return <Database className={className} />
  if (span.gen_ai_operation === 'retrieval')
    return <Globe className={className} />
  if (
    span.gen_ai_operation === 'chat' ||
    span.gen_ai_operation === 'generate_content' ||
    span.gen_ai_operation === 'text_completion'
  )
    return <MessageSquare className={className} />
  if (span.gen_ai_system) return <Cpu className={className} />
  if (span.kind === 'CLIENT') return <Globe className={className} />
  if (span.kind === 'SERVER') return <Activity className={className} />
  return <Hash className={className} />
}

function spanLabel(span: GenAiSpanDetail): string {
  if (span.tool_name) return span.tool_name
  if (span.agent_name) return span.agent_name
  return span.name
}

function cacheHitPct(trace: GenAiTraceSummary): number | null {
  const input = trace.total_input_tokens
  const cacheRead = trace.total_cache_read_input_tokens
  if (!input || !cacheRead || input === 0) return null
  return Math.round((cacheRead / input) * 100)
}

// ── SpanTreeRow ─────────────────────────────────────────────────────

function SpanTreeRow({
  node,
  depth,
  maxDuration,
  onSelect,
}: {
  node: SpanTreeNode
  depth: number
  maxDuration: number
  onSelect: (span: GenAiSpanDetail) => void
}) {
  const [open, setOpen] = useState(depth < 3)
  const span = node.span
  const hasChildren = node.children.length > 0
  const durationPct =
    maxDuration > 0 ? (span.duration_ms / maxDuration) * 100 : 0
  const isGenAi = !!span.gen_ai_system || !!span.gen_ai_operation
  const isError = span.status_code === 'ERROR'

  return (
    <>
      <div
        className={`flex items-center gap-1.5 py-1.5 px-2 hover:bg-muted/50 cursor-pointer rounded-sm group ${
          isError ? 'bg-destructive/5' : ''
        }`}
        style={{ paddingLeft: `${depth * 20 + 8}px` }}
        onClick={() => onSelect(span)}
      >
        {/* Expand/collapse toggle */}
        {hasChildren ? (
          <button
            className="h-4 w-4 shrink-0 text-muted-foreground hover:text-foreground"
            onClick={(e) => {
              e.stopPropagation()
              setOpen(!open)
            }}
          >
            <ChevronRight
              className={`h-3.5 w-3.5 transition-transform ${open ? 'rotate-90' : ''}`}
            />
          </button>
        ) : (
          <span className="w-4 shrink-0" />
        )}

        {/* Icon */}
        {spanIcon(
          span,
          `h-3.5 w-3.5 shrink-0 ${isGenAi ? 'text-primary' : 'text-muted-foreground'}`
        )}

        {/* Name */}
        <span
          className={`font-mono text-xs truncate max-w-[120px] sm:max-w-[260px] ${isGenAi ? 'text-foreground' : 'text-muted-foreground'}`}
        >
          {spanLabel(span)}
        </span>

        {/* Operation badge */}
        {span.gen_ai_operation && (
          <Badge
            variant="outline"
            className="text-[9px] px-1 py-0 h-4 shrink-0 hidden sm:inline-flex"
          >
            {span.gen_ai_operation}
          </Badge>
        )}

        {/* Model badge */}
        {span.gen_ai_model && (
          <Badge
            variant="secondary"
            className="text-[9px] px-1 py-0 h-4 shrink-0 hidden lg:inline-flex"
          >
            {span.gen_ai_model}
          </Badge>
        )}

        {/* Error indicator */}
        {isError && (
          <AlertTriangle className="h-3 w-3 text-destructive shrink-0" />
        )}

        {/* Spacer */}
        <div className="flex-1" />

        {/* Tokens */}
        {(span.input_tokens != null || span.output_tokens != null) && (
          <span className="text-[10px] text-muted-foreground tabular-nums hidden sm:inline">
            {span.input_tokens != null
              ? formatTokenCount(span.input_tokens)
              : '—'}
            {' / '}
            {span.output_tokens != null
              ? formatTokenCount(span.output_tokens)
              : '—'}
          </span>
        )}

        {/* Cache indicator */}
        {span.cache_read_input_tokens != null &&
          span.cache_read_input_tokens > 0 && (
            <TooltipProvider>
              <Tooltip>
                <TooltipTrigger asChild>
                  <Badge
                    variant="outline"
                    className="text-[9px] px-1 py-0 h-4 text-blue-500 border-blue-500/30 shrink-0"
                  >
                    cached
                  </Badge>
                </TooltipTrigger>
                <TooltipContent>
                  <p>
                    {formatTokenCount(span.cache_read_input_tokens)} tokens from
                    cache
                  </p>
                </TooltipContent>
              </Tooltip>
            </TooltipProvider>
          )}

        {/* Duration bar */}
        <div className="w-16 shrink-0 hidden md:flex items-center gap-1">
          <div className="flex-1 h-1.5 bg-muted rounded-full overflow-hidden">
            <div
              className={`h-full rounded-full ${isError ? 'bg-destructive' : isGenAi ? 'bg-primary' : 'bg-muted-foreground/40'}`}
              style={{ width: `${Math.max(durationPct, 2)}%` }}
            />
          </div>
          <span className="text-[10px] text-muted-foreground tabular-nums w-10 text-right">
            {span.duration_ms >= 1000
              ? `${(span.duration_ms / 1000).toFixed(1)}s`
              : `${span.duration_ms.toFixed(0)}ms`}
          </span>
        </div>
      </div>

      {/* Children */}
      {open &&
        hasChildren &&
        node.children.map((child) => (
          <SpanTreeRow
            key={child.span.span_id}
            node={child}
            depth={depth + 1}
            maxDuration={maxDuration}
            onSelect={onSelect}
          />
        ))}
    </>
  )
}

// ── SpanDetailSheet ─────────────────────────────────────────────────

function DetailRow({
  label,
  value,
  mono,
}: {
  label: string
  value: React.ReactNode
  mono?: boolean
}) {
  if (value == null || value === '' || value === '—') return null
  return (
    <div className="flex justify-between items-start gap-4 py-1.5">
      <span className="text-xs text-muted-foreground shrink-0">{label}</span>
      <span
        className={`text-xs text-right break-all ${mono ? 'font-mono' : ''}`}
      >
        {value}
      </span>
    </div>
  )
}

function DetailSection({
  title,
  children,
}: {
  title: string
  children: React.ReactNode
}) {
  const hasContent = (() => {
    const arr = Array.isArray(children) ? children : [children]
    return arr.some((c) => c != null && c !== false)
  })()
  if (!hasContent) return null
  return (
    <div>
      <h4 className="text-xs font-semibold text-muted-foreground uppercase tracking-wider mb-1">
        {title}
      </h4>
      <div className="divide-y divide-border">{children}</div>
    </div>
  )
}

interface ContentBlock {
  type: string
  text?: string
  thinking?: string
  id?: string
  name?: string
  input?: Record<string, unknown> | string
}

interface ChatMessage {
  role: string
  content?: string | ContentBlock[] | null
  tool_calls?: Array<{
    id?: string
    function?: { name?: string; arguments?: string }
  }>
  tool_call_id?: string
}

function ChatMessageBubble({ msg }: { msg: ChatMessage }) {
  const isUser = msg.role === 'user'
  const isTool = msg.role === 'tool'
  const isSystem = msg.role === 'system'

  // Check if content is an array of content blocks (Anthropic format)
  const contentBlocks = Array.isArray(msg.content) ? msg.content : null
  const textContent = typeof msg.content === 'string' ? msg.content : null

  // Extract thinking blocks and text blocks separately
  const thinkingBlocks =
    contentBlocks?.filter((b) => b.type === 'thinking') ?? []
  const textBlocks = contentBlocks?.filter((b) => b.type === 'text') ?? []
  const toolUseBlocks =
    contentBlocks?.filter((b) => b.type === 'tool_use') ?? []
  const toolResultBlocks =
    contentBlocks?.filter((b) => b.type === 'tool_result') ?? []

  return (
    <div className="space-y-1.5">
      {/* Thinking blocks - shown before the main message */}
      {thinkingBlocks.map((block, i) => (
        <div key={`thinking-${i}`} className="flex justify-start">
          <div className="max-w-[90%] rounded-lg px-3 py-2 text-xs bg-violet-500/10 border border-violet-500/20">
            <div className="flex items-center gap-1.5 mb-1">
              <span className="text-[10px] font-semibold uppercase text-violet-500">
                thinking
              </span>
            </div>
            <div className="whitespace-pre-wrap break-words text-muted-foreground italic">
              {block.thinking || block.text}
            </div>
          </div>
        </div>
      ))}

      {/* Main message bubble */}
      <div className={`flex ${isUser ? 'justify-end' : 'justify-start'}`}>
        <div
          className={`max-w-[90%] rounded-lg px-3 py-2 text-xs ${
            isUser
              ? 'bg-primary text-primary-foreground'
              : isTool
                ? 'bg-amber-500/10 border border-amber-500/20'
                : isSystem
                  ? 'bg-muted border border-border italic'
                  : 'bg-muted'
          }`}
        >
          <div className="flex items-center gap-1.5 mb-1">
            <span
              className={`text-[10px] font-semibold uppercase ${
                isUser ? 'text-primary-foreground/70' : 'text-muted-foreground'
              }`}
            >
              {msg.role}
              {isTool && msg.tool_call_id && (
                <span className="font-mono font-normal ml-1">
                  ({msg.tool_call_id})
                </span>
              )}
            </span>
          </div>
          {/* String content */}
          {textContent && (
            <div className="whitespace-pre-wrap break-words">{textContent}</div>
          )}
          {/* Content block text */}
          {textBlocks.map((block, i) => (
            <div key={`text-${i}`} className="whitespace-pre-wrap break-words">
              {block.text}
            </div>
          ))}
          {/* Tool result blocks (Anthropic format) */}
          {toolResultBlocks.map((block, i) => (
            <div
              key={`result-${i}`}
              className="whitespace-pre-wrap break-words"
            >
              {typeof block.text === 'string'
                ? block.text
                : JSON.stringify(block.input ?? block.text, null, 2)}
            </div>
          ))}
          {/* Tool calls from OpenAI format */}
          {msg.tool_calls && msg.tool_calls.length > 0 && (
            <div className="space-y-1 mt-1">
              {msg.tool_calls.map((tc, j) => (
                <div
                  key={j}
                  className="bg-background/50 rounded p-1.5 font-mono text-[10px]"
                >
                  <span className="text-amber-500">{tc.function?.name}</span>
                  {tc.function?.arguments && (
                    <pre className="text-muted-foreground mt-0.5 whitespace-pre-wrap">
                      {tc.function.arguments}
                    </pre>
                  )}
                </div>
              ))}
            </div>
          )}
          {/* Tool use blocks from Anthropic format */}
          {toolUseBlocks.map((block, j) => (
            <div
              key={`tool-use-${j}`}
              className="bg-background/50 rounded p-1.5 font-mono text-[10px] mt-1"
            >
              <span className="text-amber-500">{block.name}</span>
              {block.id && (
                <span className="text-muted-foreground ml-1">({block.id})</span>
              )}
              {block.input && (
                <pre className="text-muted-foreground mt-0.5 whitespace-pre-wrap">
                  {typeof block.input === 'string'
                    ? block.input
                    : JSON.stringify(block.input, null, 2)}
                </pre>
              )}
            </div>
          ))}
        </div>
      </div>
    </div>
  )
}

interface ToolSpanInfo {
  tool_name: string | null
  tool_call_id: string | null
  tool_call_arguments: string | null
  tool_call_result: string | null
  duration_ms: number
}

function FullConversationView({
  systemInstructions,
  inputMessages,
  outputMessages,
  toolSpans,
}: {
  systemInstructions?: string | null
  inputMessages?: string | null
  outputMessages?: string | null
  toolSpans?: ToolSpanInfo[]
}) {
  const allMessages: ChatMessage[] = []

  // 1. System prompt first
  if (systemInstructions) {
    allMessages.push({ role: 'system', content: systemInstructions })
  }

  // 2. Input messages
  if (inputMessages) {
    try {
      const parsed = JSON.parse(inputMessages)
      if (Array.isArray(parsed)) allMessages.push(...parsed)
    } catch {
      allMessages.push({ role: 'user', content: inputMessages })
    }
  }

  // 3. Output messages
  if (outputMessages) {
    try {
      const parsed = JSON.parse(outputMessages)
      if (Array.isArray(parsed)) allMessages.push(...parsed)
    } catch {
      allMessages.push({ role: 'assistant', content: outputMessages })
    }
  }

  // 4. Tool execution spans (from sibling execute_tool spans)
  if (toolSpans && toolSpans.length > 0) {
    for (const ts of toolSpans) {
      // Show tool call as a tool-execution bubble
      allMessages.push({
        role: 'tool_execution',
        content: [
          {
            type: 'tool_exec',
            text: ts.tool_call_result || undefined,
            name: ts.tool_name || undefined,
          },
        ] as ContentBlock[],
        tool_call_id: ts.tool_call_id || undefined,
      })
    }
  }

  if (allMessages.length === 0) return null

  return (
    <div className="space-y-2">
      {allMessages.map((msg, i) => {
        // Special rendering for tool execution spans
        if (msg.role === 'tool_execution') {
          const blocks = Array.isArray(msg.content) ? msg.content : []
          const block = blocks[0]
          return (
            <div key={i} className="flex justify-start">
              <div className="max-w-[90%] rounded-lg px-3 py-2 text-xs bg-emerald-500/10 border border-emerald-500/20">
                <div className="flex items-center gap-1.5 mb-1">
                  <span className="text-[10px] font-semibold uppercase text-emerald-600">
                    tool execution
                  </span>
                  {block?.name && (
                    <span className="font-mono text-[10px] text-emerald-500">
                      {block.name}
                    </span>
                  )}
                  {msg.tool_call_id && (
                    <span className="font-mono text-[10px] text-muted-foreground">
                      ({msg.tool_call_id})
                    </span>
                  )}
                </div>
                {block?.text && (
                  <pre className="whitespace-pre-wrap break-words font-mono text-[10px] text-muted-foreground mt-1">
                    {(() => {
                      try {
                        return JSON.stringify(JSON.parse(block.text), null, 2)
                      } catch {
                        return block.text
                      }
                    })()}
                  </pre>
                )}
              </div>
            </div>
          )
        }
        return <ChatMessageBubble key={i} msg={msg} />
      })}
    </div>
  )
}

function JsonBlock({ label, value }: { label: string; value: string | null }) {
  const [open, setOpen] = useState(false)
  if (!value) return null
  let formatted: string
  try {
    formatted = JSON.stringify(JSON.parse(value), null, 2)
  } catch {
    formatted = value
  }
  return (
    <Collapsible open={open} onOpenChange={setOpen} className="py-1.5">
      <CollapsibleTrigger className="flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground">
        <ChevronRight
          className={`h-3 w-3 transition-transform ${open ? 'rotate-90' : ''}`}
        />
        {label}
      </CollapsibleTrigger>
      <CollapsibleContent>
        <pre className="mt-1 p-2 bg-muted rounded text-[10px] font-mono overflow-x-auto max-h-[200px] overflow-y-auto">
          {formatted}
        </pre>
      </CollapsibleContent>
    </Collapsible>
  )
}

function SpanDetailSheet({
  span,
  open,
  onOpenChange,
  allSpans,
}: {
  span: GenAiSpanDetail | null
  open: boolean
  onOpenChange: (open: boolean) => void
  allSpans?: GenAiSpanDetail[]
}) {
  if (!span) return null
  const isError = span.status_code === 'ERROR'

  const totalTokens = (span.input_tokens ?? 0) + (span.output_tokens ?? 0)
  const cacheTokens =
    (span.cache_read_input_tokens ?? 0) +
    (span.cache_creation_input_tokens ?? 0)

  // Find sibling tool execution spans (children of the same parent, or children of this span)
  const siblingToolSpans: ToolSpanInfo[] = (allSpans ?? [])
    .filter(
      (s) =>
        s.span_id !== span.span_id &&
        (s.gen_ai_operation === 'execute_tool' || s.tool_name) &&
        (s.parent_span_id === span.parent_span_id ||
          s.parent_span_id === span.span_id)
    )
    .map((s) => ({
      tool_name: s.tool_name,
      tool_call_id: s.tool_call_id,
      tool_call_arguments: s.tool_call_arguments,
      tool_call_result: s.tool_call_result,
      duration_ms: s.duration_ms,
    }))

  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent className="w-full sm:max-w-lg overflow-y-auto">
        <SheetHeader className="pb-4">
          <div className="flex items-center gap-2">
            {spanIcon(span, 'h-4 w-4 text-primary')}
            <SheetTitle className="text-sm font-mono">
              {spanLabel(span)}
            </SheetTitle>
          </div>
          <div className="flex items-center gap-2 flex-wrap">
            {span.gen_ai_operation && (
              <Badge variant="outline" className="text-[10px]">
                {span.gen_ai_operation}
              </Badge>
            )}
            {span.gen_ai_system && (
              <Badge variant="secondary" className="text-[10px]">
                {span.gen_ai_system}
              </Badge>
            )}
            <Badge
              variant={isError ? 'destructive' : 'default'}
              className={!isError ? 'bg-green-500/15 text-green-500' : ''}
            >
              {span.status_code}
            </Badge>
            {isError && span.error_type && (
              <Badge variant="destructive" className="text-[10px]">
                {span.error_type}
              </Badge>
            )}
          </div>
        </SheetHeader>

        <div className="space-y-5">
          {/* Token summary cards */}
          {totalTokens > 0 && (
            <div className="grid grid-cols-2 gap-2">
              <Card className="p-3">
                <div className="text-[10px] text-muted-foreground uppercase">
                  Input
                </div>
                <div className="text-lg font-bold tabular-nums">
                  {formatTokenCount(span.input_tokens ?? 0)}
                </div>
                {span.cache_read_input_tokens != null &&
                  span.cache_read_input_tokens > 0 && (
                    <div className="text-[10px] text-blue-500">
                      {formatTokenCount(span.cache_read_input_tokens)} cached
                    </div>
                  )}
                {span.cache_creation_input_tokens != null &&
                  span.cache_creation_input_tokens > 0 && (
                    <div className="text-[10px] text-amber-500">
                      {formatTokenCount(span.cache_creation_input_tokens)} cache
                      write
                    </div>
                  )}
              </Card>
              <Card className="p-3">
                <div className="text-[10px] text-muted-foreground uppercase">
                  Output
                </div>
                <div className="text-lg font-bold tabular-nums">
                  {formatTokenCount(span.output_tokens ?? 0)}
                </div>
                {span.output_type && (
                  <div className="text-[10px] text-muted-foreground">
                    {span.output_type}
                  </div>
                )}
              </Card>
            </div>
          )}

          {/* Cache ratio bar */}
          {cacheTokens > 0 &&
            span.input_tokens != null &&
            span.input_tokens > 0 && (
              <div>
                <div className="flex justify-between text-[10px] text-muted-foreground mb-1">
                  <span>Cache hit ratio</span>
                  <span>
                    {Math.round(
                      ((span.cache_read_input_tokens ?? 0) /
                        span.input_tokens) *
                        100
                    )}
                    %
                  </span>
                </div>
                <Progress
                  value={
                    ((span.cache_read_input_tokens ?? 0) / span.input_tokens) *
                    100
                  }
                  className="h-1.5"
                />
              </div>
            )}

          <Separator />

          {/* Identification */}
          <DetailSection title="Identification">
            <DetailRow label="Span ID" value={span.span_id} mono />
            <DetailRow label="Parent Span" value={span.parent_span_id} mono />
            <DetailRow label="Kind" value={span.kind} />
            <DetailRow
              label="Duration"
              value={`${span.duration_ms.toFixed(1)}ms`}
            />
            <DetailRow
              label="Start"
              value={new Date(span.start_time).toLocaleString()}
            />
          </DetailSection>

          {/* Model */}
          <DetailSection title="Model">
            <DetailRow label="Requested Model" value={span.gen_ai_model} mono />
            <DetailRow
              label="Response Model"
              value={span.gen_ai_response_model}
              mono
            />
            <DetailRow label="Response ID" value={span.response_id} mono />
            <DetailRow
              label="Finish Reasons"
              value={span.response_finish_reasons?.join(', ')}
            />
            <DetailRow
              label="Conversation ID"
              value={span.conversation_id}
              mono
            />
          </DetailSection>

          {/* Request Parameters */}
          <DetailSection title="Request Parameters">
            <DetailRow
              label="Temperature"
              value={span.request_temperature?.toString()}
            />
            <DetailRow
              label="Max Tokens"
              value={span.request_max_tokens?.toString()}
            />
            <DetailRow label="Top P" value={span.request_top_p?.toString()} />
            <DetailRow label="Top K" value={span.request_top_k?.toString()} />
            <DetailRow
              label="Frequency Penalty"
              value={span.request_frequency_penalty?.toString()}
            />
            <DetailRow
              label="Presence Penalty"
              value={span.request_presence_penalty?.toString()}
            />
            <DetailRow
              label="Stop Sequences"
              value={span.request_stop_sequences?.join(', ')}
            />
            <DetailRow label="Seed" value={span.request_seed?.toString()} />
            <DetailRow
              label="Choice Count"
              value={span.request_choice_count?.toString()}
            />
          </DetailSection>

          {/* Agent */}
          {(span.agent_name || span.agent_id) && (
            <DetailSection title="Agent">
              <DetailRow label="Name" value={span.agent_name} />
              <DetailRow label="ID" value={span.agent_id} mono />
              <DetailRow label="Version" value={span.agent_version} />
              <DetailRow label="Description" value={span.agent_description} />
            </DetailSection>
          )}

          {/* Tool */}
          {(span.tool_name || span.tool_call_id) && (
            <DetailSection title="Tool Execution">
              <DetailRow label="Tool Name" value={span.tool_name} />
              <DetailRow label="Call ID" value={span.tool_call_id} mono />
              <DetailRow label="Type" value={span.tool_type} />
              <DetailRow label="Description" value={span.tool_description} />
            </DetailSection>
          )}

          {/* Embeddings */}
          {span.embeddings_dimension_count != null && (
            <DetailSection title="Embeddings">
              <DetailRow
                label="Dimensions"
                value={span.embeddings_dimension_count.toString()}
              />
              <DetailRow
                label="Encoding Formats"
                value={span.request_encoding_formats?.join(', ')}
              />
            </DetailSection>
          )}

          {/* Retrieval */}
          {span.data_source_id && (
            <DetailSection title="Retrieval">
              <DetailRow label="Data Source" value={span.data_source_id} mono />
            </DetailSection>
          )}

          {/* Server */}
          {(span.server_address || span.server_port) && (
            <DetailSection title="Server">
              <DetailRow label="Address" value={span.server_address} mono />
              <DetailRow label="Port" value={span.server_port?.toString()} />
            </DetailSection>
          )}

          {/* Provider-Specific */}
          {(span.openai_api_type ||
            span.openai_system_fingerprint ||
            span.aws_bedrock_guardrail_id ||
            span.azure_resource_provider_namespace) && (
            <DetailSection title="Provider Specific">
              <DetailRow label="OpenAI API Type" value={span.openai_api_type} />
              <DetailRow
                label="Request Service Tier"
                value={span.openai_request_service_tier}
              />
              <DetailRow
                label="Response Service Tier"
                value={span.openai_response_service_tier}
              />
              <DetailRow
                label="System Fingerprint"
                value={span.openai_system_fingerprint}
                mono
              />
              <DetailRow
                label="Bedrock Guardrail"
                value={span.aws_bedrock_guardrail_id}
                mono
              />
              <DetailRow
                label="Bedrock Knowledge Base"
                value={span.aws_bedrock_knowledge_base_id}
                mono
              />
              <DetailRow
                label="Azure Namespace"
                value={span.azure_resource_provider_namespace}
              />
            </DetailSection>
          )}

          {/* Conversation / Messages */}
          {(span.system_instructions ||
            span.input_messages ||
            span.output_messages) && (
            <>
              <Separator />
              <div>
                <h4 className="text-xs font-semibold text-muted-foreground uppercase tracking-wider mb-2">
                  Conversation
                </h4>
                <FullConversationView
                  systemInstructions={span.system_instructions}
                  inputMessages={span.input_messages}
                  outputMessages={span.output_messages}
                  toolSpans={siblingToolSpans}
                />
              </div>
            </>
          )}

          {/* Tool Content */}
          {(span.tool_call_arguments ||
            span.tool_call_result ||
            span.tool_definitions) && (
            <>
              <Separator />
              <div>
                <h4 className="text-xs font-semibold text-muted-foreground uppercase tracking-wider mb-1">
                  Tool Content
                </h4>
                <JsonBlock label="Arguments" value={span.tool_call_arguments} />
                <JsonBlock label="Result" value={span.tool_call_result} />
                <JsonBlock label="Definitions" value={span.tool_definitions} />
              </div>
            </>
          )}

          {/* Retrieval Content */}
          {(span.retrieval_query_text || span.retrieval_documents) && (
            <>
              <Separator />
              <div>
                <h4 className="text-xs font-semibold text-muted-foreground uppercase tracking-wider mb-1">
                  Retrieval
                </h4>
                {span.retrieval_query_text && (
                  <DetailRow label="Query" value={span.retrieval_query_text} />
                )}
                <JsonBlock label="Documents" value={span.retrieval_documents} />
              </div>
            </>
          )}
        </div>
      </SheetContent>
    </Sheet>
  )
}

// ── Invocations & conversation view ─────────────────────────────────
//
// The trace's LLM calls surfaced directly — each "invocation" (chat /
// generate / agent / embeddings span) rendered as an expanded card with its
// conversation inline, so the messages are readable without hunting through
// the span tree and opening a span sheet.

const LLM_OPS = new Set([
  'chat',
  'generate_content',
  'text_completion',
  'completion',
  'generate',
  'invoke_agent',
  'create_agent',
  'embeddings',
])

function isToolSpan(s: GenAiSpanDetail): boolean {
  return s.gen_ai_operation === 'execute_tool' || !!s.tool_name
}

function spanHasContent(s: GenAiSpanDetail): boolean {
  return !!(s.system_instructions || s.input_messages || s.output_messages)
}

// A span worth showing as its own conversation card: an LLM/agent call, not a
// tool execution (those nest under their parent call).
function isInvocationSpan(s: GenAiSpanDetail): boolean {
  if (isToolSpan(s)) return false
  return (
    spanHasContent(s) ||
    s.gen_ai_model != null ||
    s.input_tokens != null ||
    s.output_tokens != null ||
    (!!s.gen_ai_operation && LLM_OPS.has(s.gen_ai_operation))
  )
}

interface InvocationSet {
  invocations: GenAiSpanDetail[]
  toolByInvocation: Map<string, ToolSpanInfo[]>
  hasAnyContent: boolean
}

// Pick the invocation spans and attach each tool execution to its call.
//
// Instrumentations often emit a parent + child pair for one logical call
// (e.g. the Vercel AI SDK's `ai.streamText` wrapping `ai.streamText.doStream`).
// We prefer the span that actually carries the messages and drop token-only
// spans that sit in the same ancestry chain, so one LLM call renders as one
// card instead of two near-duplicates.
function computeInvocations(spans: GenAiSpanDetail[]): InvocationSet {
  const byId = new Map(spans.map((s) => [s.span_id, s]))
  const isAncestor = (ancestorId: string, node: GenAiSpanDetail): boolean => {
    let cur = node.parent_span_id ?? null
    let guard = 0
    while (cur && guard++ < 1000) {
      if (cur === ancestorId) return true
      cur = byId.get(cur)?.parent_span_id ?? null
    }
    return false
  }

  const candidates = spans.filter(isInvocationSpan)
  const contentSpans = candidates.filter(spanHasContent)
  const contentIds = new Set(contentSpans.map((s) => s.span_id))

  // Content-bearing calls are always shown. Then add token/model-only calls
  // that aren't part of a content call's chain (so we never hide a real call,
  // but also never double up on one that's already represented).
  const chosen: GenAiSpanDetail[] = [...contentSpans]
  for (const c of candidates) {
    if (contentIds.has(c.span_id)) continue
    const overlapsContent = contentSpans.some(
      (cs) => isAncestor(cs.span_id, c) || isAncestor(c.span_id, cs)
    )
    if (overlapsContent) continue
    chosen.push(c)
  }
  // Among token-only calls, keep the topmost of any parent/child pair.
  const deduped = chosen.filter((s) => {
    if (contentIds.has(s.span_id)) return true
    return !chosen.some(
      (o) => o.span_id !== s.span_id && isAncestor(o.span_id, s)
    )
  })
  deduped.sort(
    (a, b) =>
      new Date(a.start_time).getTime() - new Date(b.start_time).getTime()
  )

  // Attach each tool-execution span to the call it belongs to (its parent, or a
  // call it shares a parent with), assigning each tool span only once.
  const toolSpans = spans.filter(isToolSpan)
  const used = new Set<string>()
  const toolByInvocation = new Map<string, ToolSpanInfo[]>()
  for (const inv of deduped) {
    const list: ToolSpanInfo[] = []
    for (const t of toolSpans) {
      if (used.has(t.span_id)) continue
      if (
        t.parent_span_id === inv.span_id ||
        (inv.parent_span_id != null && t.parent_span_id === inv.parent_span_id)
      ) {
        used.add(t.span_id)
        list.push({
          tool_name: t.tool_name,
          tool_call_id: t.tool_call_id,
          tool_call_arguments: t.tool_call_arguments,
          tool_call_result: t.tool_call_result,
          duration_ms: t.duration_ms,
        })
      }
    }
    if (list.length) toolByInvocation.set(inv.span_id, list)
  }

  return {
    invocations: deduped,
    toolByInvocation,
    hasAnyContent: contentSpans.length > 0,
  }
}

function fmtSpanDuration(ms: number): string {
  return ms >= 1000 ? `${(ms / 1000).toFixed(1)}s` : `${ms.toFixed(0)}ms`
}

// One LLM call: header (model, provider, tokens, duration, finish reason) plus
// its conversation expanded inline. Clicking the header collapses it; "Details"
// opens the full span sheet for raw attributes.
function InvocationCard({
  span,
  index,
  toolSpans,
  onOpenSpan,
}: {
  span: GenAiSpanDetail
  index: number
  toolSpans: ToolSpanInfo[]
  onOpenSpan: (span: GenAiSpanDetail) => void
}) {
  const [open, setOpen] = useState(true)
  const isError = span.status_code === 'ERROR'
  const hasContent = spanHasContent(span)
  const model = span.gen_ai_response_model || span.gen_ai_model
  const cacheRead = span.cache_read_input_tokens ?? 0

  return (
    <Card
      className={`overflow-hidden ${isError ? 'border-destructive/40' : ''}`}
    >
      <Collapsible open={open} onOpenChange={setOpen}>
        <div className="flex items-start gap-2 p-3">
          <CollapsibleTrigger asChild>
            <button className="flex min-w-0 flex-1 items-start gap-2 text-left">
              <ChevronDown
                className={`mt-1 h-4 w-4 shrink-0 text-muted-foreground transition-transform ${open ? '' : '-rotate-90'}`}
              />
              <span
                className={`mt-0.5 flex h-6 w-6 shrink-0 items-center justify-center rounded-md ${
                  isError
                    ? 'bg-destructive/10 text-destructive'
                    : 'bg-primary/10 text-primary'
                }`}
              >
                {spanIcon(span, 'h-3.5 w-3.5')}
              </span>
              <div className="min-w-0 flex-1 space-y-1">
                <div className="flex flex-wrap items-center gap-1.5">
                  <span className="text-[10px] font-semibold tabular-nums text-muted-foreground">
                    #{index + 1}
                  </span>
                  <span className="truncate font-mono text-xs font-medium">
                    {spanLabel(span)}
                  </span>
                  {span.gen_ai_operation && (
                    <Badge variant="outline" className="h-4 px-1 text-[9px]">
                      {span.gen_ai_operation}
                    </Badge>
                  )}
                  {span.gen_ai_system && (
                    <Badge variant="secondary" className="h-4 px-1 text-[9px]">
                      {span.gen_ai_system}
                    </Badge>
                  )}
                  {model && (
                    <Badge
                      variant="secondary"
                      className="h-4 px-1 font-mono text-[9px]"
                    >
                      {model}
                    </Badge>
                  )}
                  {isError && (
                    <Badge
                      variant="destructive"
                      className="h-4 px-1 text-[9px]"
                    >
                      {span.error_type || 'error'}
                    </Badge>
                  )}
                </div>
                <div className="flex flex-wrap items-center gap-x-3 gap-y-0.5 text-[10px] tabular-nums text-muted-foreground">
                  <span className="inline-flex items-center gap-1">
                    <Clock className="h-3 w-3" />
                    {fmtSpanDuration(span.duration_ms)}
                  </span>
                  {(span.input_tokens != null ||
                    span.output_tokens != null) && (
                    <span>
                      {span.input_tokens != null
                        ? formatTokenCount(span.input_tokens)
                        : '—'}{' '}
                      in
                      {' / '}
                      {span.output_tokens != null
                        ? formatTokenCount(span.output_tokens)
                        : '—'}{' '}
                      out
                    </span>
                  )}
                  {cacheRead > 0 && (
                    <span className="text-blue-500">
                      {formatTokenCount(cacheRead)} cached
                    </span>
                  )}
                  {span.response_finish_reasons &&
                    span.response_finish_reasons.length > 0 && (
                      <span>
                        finish: {span.response_finish_reasons.join(', ')}
                      </span>
                    )}
                </div>
              </div>
            </button>
          </CollapsibleTrigger>
          <Button
            variant="ghost"
            size="sm"
            className="h-7 shrink-0 gap-1 px-2 text-xs text-muted-foreground"
            onClick={() => onOpenSpan(span)}
          >
            <Info className="h-3.5 w-3.5" />
            <span className="hidden sm:inline">Details</span>
          </Button>
        </div>
        <CollapsibleContent>
          <div className="border-t bg-muted/20 p-3">
            {hasContent ? (
              <FullConversationView
                systemInstructions={span.system_instructions}
                inputMessages={span.input_messages}
                outputMessages={span.output_messages}
                toolSpans={toolSpans}
              />
            ) : (
              <p className="text-xs text-muted-foreground">
                No message content was captured for this call. Enable it by
                exporting{' '}
                <code className="font-mono text-[11px]">
                  gen_ai.input.messages
                </code>{' '}
                /{' '}
                <code className="font-mono text-[11px]">
                  gen_ai.output.messages
                </code>{' '}
                (opt-in in the OpenTelemetry GenAI semantic conventions), or
                open{' '}
                <button
                  type="button"
                  className="underline underline-offset-2 hover:text-foreground"
                  onClick={() => onOpenSpan(span)}
                >
                  span details
                </button>
                .
              </p>
            )}
          </div>
        </CollapsibleContent>
      </Collapsible>
    </Card>
  )
}

// The default tab of a trace: its AI invocations, each with its conversation
// expanded inline. Reads top-to-bottom like a transcript.
function TraceConversationView({
  spans,
  onOpenSpan,
}: {
  spans: GenAiSpanDetail[]
  onOpenSpan: (span: GenAiSpanDetail) => void
}) {
  const { invocations, toolByInvocation, hasAnyContent } = useMemo(
    () => computeInvocations(spans),
    [spans]
  )

  if (invocations.length === 0) {
    return (
      <EmptyState
        icon={MessageSquare}
        title="No AI invocations in this trace"
        description="This trace has no spans with gen_ai.* attributes. Open the Span tree tab to inspect all spans."
      />
    )
  }

  return (
    <div className="space-y-3">
      {!hasAnyContent && (
        <div className="rounded-lg border border-amber-500/30 bg-amber-500/5 p-3 text-xs text-muted-foreground">
          Message content isn&apos;t captured for these calls. You can see each
          call&apos;s model, tokens, and timing below — to read the actual
          prompts and responses, export{' '}
          <code className="font-mono text-[11px]">gen_ai.input.messages</code>{' '}
          and{' '}
          <code className="font-mono text-[11px]">gen_ai.output.messages</code>{' '}
          (opt-in content capture).
        </div>
      )}
      {invocations.map((span, i) => (
        <InvocationCard
          key={span.span_id}
          span={span}
          index={i}
          toolSpans={toolByInvocation.get(span.span_id) ?? []}
          onOpenSpan={onOpenSpan}
        />
      ))}
    </div>
  )
}

// ── Trace Detail View ───────────────────────────────────────────────

function TraceDetailView({
  traceId,
  traceDetail,
  isLoading: detailLoading,
  events,
  onBack,
}: {
  traceId: string
  traceDetail: GenAiTraceDetailResponse | undefined
  isLoading: boolean
  events: GenAiEvent[]
  onBack: () => void
}) {
  const [selectedSpan, setSelectedSpan] = useState<GenAiSpanDetail | null>(null)
  const [sheetOpen, setSheetOpen] = useState(false)

  const tree = useMemo(
    () => (traceDetail ? buildSpanTree(traceDetail.spans) : []),
    [traceDetail]
  )

  const maxDuration = useMemo(
    () =>
      traceDetail
        ? Math.max(...traceDetail.spans.map((s) => s.duration_ms), 1)
        : 1,
    [traceDetail]
  )

  // Summary stats
  const stats = useMemo(() => {
    if (!traceDetail) return null
    const spans = traceDetail.spans
    const genAiSpans = spans.filter(
      (s) => s.gen_ai_system || s.gen_ai_operation
    )
    const totalInput = spans.reduce((s, sp) => s + (sp.input_tokens ?? 0), 0)
    const totalOutput = spans.reduce((s, sp) => s + (sp.output_tokens ?? 0), 0)
    const totalCacheRead = spans.reduce(
      (s, sp) => s + (sp.cache_read_input_tokens ?? 0),
      0
    )
    const totalCacheCreate = spans.reduce(
      (s, sp) => s + (sp.cache_creation_input_tokens ?? 0),
      0
    )
    const errors = spans.filter((s) => s.status_code === 'ERROR')
    const tools = spans.filter(
      (s) => s.gen_ai_operation === 'execute_tool' || s.tool_name
    )
    const agents = spans.filter(
      (s) => s.agent_name || s.gen_ai_operation === 'invoke_agent'
    )
    const rootDuration = tree[0]?.span.duration_ms ?? 0
    return {
      genAiSpans,
      totalInput,
      totalOutput,
      totalCacheRead,
      totalCacheCreate,
      errors,
      tools,
      agents,
      rootDuration,
    }
  }, [traceDetail, tree])

  // Count of AI invocations for the Conversation tab label.
  const invocationCount = useMemo(
    () =>
      traceDetail
        ? computeInvocations(traceDetail.spans).invocations.length
        : 0,
    [traceDetail]
  )

  const handleSelect = (span: GenAiSpanDetail) => {
    setSelectedSpan(span)
    setSheetOpen(true)
  }

  return (
    <div className="space-y-4">
      <div className="flex items-center gap-2">
        <Button variant="ghost" size="sm" onClick={onBack}>
          <ArrowLeft className="h-4 w-4 mr-1" />
          Back to traces
        </Button>
        <span className="text-sm text-muted-foreground font-mono truncate">
          {traceId}
        </span>
      </div>

      {detailLoading ? (
        <div className="space-y-2">
          <Skeleton className="h-20 w-full" />
          <Skeleton className="h-12 w-full" />
          <Skeleton className="h-12 w-full" />
        </div>
      ) : !traceDetail || traceDetail.spans.length === 0 ? (
        <EmptyState
          icon={Bot}
          title="No spans found"
          description="This trace has no spans."
        />
      ) : (
        <>
          {/* Summary stat cards */}
          {stats && (
            <div className="grid gap-3 grid-cols-2 md:grid-cols-4 lg:grid-cols-6">
              <Card className="p-3">
                <div className="text-[10px] text-muted-foreground uppercase">
                  Total Spans
                </div>
                <div className="text-xl font-bold">
                  {traceDetail.span_count}
                </div>
                <div className="text-[10px] text-muted-foreground">
                  {stats.genAiSpans.length} GenAI
                </div>
              </Card>
              <Card className="p-3">
                <div className="text-[10px] text-muted-foreground uppercase">
                  Duration
                </div>
                <div className="text-xl font-bold">
                  {stats.rootDuration >= 1000
                    ? `${(stats.rootDuration / 1000).toFixed(1)}s`
                    : `${stats.rootDuration.toFixed(0)}ms`}
                </div>
              </Card>
              <Card className="p-3">
                <div className="text-[10px] text-muted-foreground uppercase">
                  Input Tokens
                </div>
                <div className="text-xl font-bold tabular-nums">
                  {formatTokenCount(stats.totalInput)}
                </div>
                {stats.totalCacheRead > 0 && (
                  <div className="text-[10px] text-blue-500">
                    {formatTokenCount(stats.totalCacheRead)} cached
                  </div>
                )}
              </Card>
              <Card className="p-3">
                <div className="text-[10px] text-muted-foreground uppercase">
                  Output Tokens
                </div>
                <div className="text-xl font-bold tabular-nums">
                  {formatTokenCount(stats.totalOutput)}
                </div>
              </Card>
              {stats.tools.length > 0 && (
                <Card className="p-3">
                  <div className="text-[10px] text-muted-foreground uppercase">
                    Tool Calls
                  </div>
                  <div className="text-xl font-bold">{stats.tools.length}</div>
                </Card>
              )}
              {stats.errors.length > 0 && (
                <Card className="p-3 border-destructive/50">
                  <div className="text-[10px] text-destructive uppercase">
                    Errors
                  </div>
                  <div className="text-xl font-bold text-destructive">
                    {stats.errors.length}
                  </div>
                  <div className="text-[10px] text-destructive/70">
                    {stats.errors[0].error_type ?? 'Unknown'}
                  </div>
                </Card>
              )}
            </div>
          )}

          {/* Invocations (default) + raw span tree */}
          <Tabs defaultValue="conversation" className="w-full">
            <TabsList>
              <TabsTrigger value="conversation" className="gap-1.5">
                <MessageSquare className="h-3.5 w-3.5" />
                Conversation
                {invocationCount > 0 && (
                  <span className="text-xs text-muted-foreground">
                    {invocationCount}
                  </span>
                )}
              </TabsTrigger>
              <TabsTrigger value="tree" className="gap-1.5">
                <ListTree className="h-3.5 w-3.5" />
                Span tree
                <span className="text-xs text-muted-foreground">
                  {traceDetail.span_count}
                </span>
              </TabsTrigger>
            </TabsList>

            {/* Default: the AI invocations, each with its conversation inline. */}
            <TabsContent value="conversation" className="mt-4">
              <TraceConversationView
                spans={traceDetail.spans}
                onOpenSpan={handleSelect}
              />
            </TabsContent>

            {/* Raw span tree — click any span for full details. */}
            <TabsContent value="tree" className="mt-4">
              <Card>
                <CardContent className="p-0">
                  <ScrollArea className="max-h-[600px]">
                    <div className="py-1">
                      {tree.map((node) => (
                        <SpanTreeRow
                          key={node.span.span_id}
                          node={node}
                          depth={0}
                          maxDuration={maxDuration}
                          onSelect={handleSelect}
                        />
                      ))}
                    </div>
                  </ScrollArea>
                </CardContent>
              </Card>
            </TabsContent>
          </Tabs>

          {/* Events */}
          {events.length > 0 && (
            <Card>
              <CardHeader className="pb-2">
                <CardTitle className="text-base">
                  Events ({events.length})
                </CardTitle>
              </CardHeader>
              <CardContent>
                <div className="space-y-2">
                  {events.map((event, i) => (
                    <div
                      key={`${event.span_id}-${i}`}
                      className="flex items-start gap-3 py-2 border-b border-border last:border-0"
                    >
                      <Info className="h-3.5 w-3.5 text-muted-foreground mt-0.5 shrink-0" />
                      <div className="flex-1 min-w-0">
                        <div className="flex items-center gap-2 flex-wrap">
                          <span className="font-mono text-xs">
                            {event.event_name}
                          </span>
                          <span className="text-[10px] text-muted-foreground">
                            {new Date(event.timestamp).toLocaleTimeString()}
                          </span>
                        </div>
                        {Object.keys(event.attributes).length > 0 && (
                          <div className="mt-1 flex flex-wrap gap-1">
                            {Object.entries(event.attributes)
                              .slice(0, 6)
                              .map(([k, v]) => (
                                <Badge
                                  key={k}
                                  variant="outline"
                                  className="text-[9px] px-1 py-0 h-4 font-mono"
                                >
                                  {k.replace('gen_ai.', '')}:{' '}
                                  {v.length > 30 ? v.slice(0, 30) + '...' : v}
                                </Badge>
                              ))}
                          </div>
                        )}
                      </div>
                    </div>
                  ))}
                </div>
              </CardContent>
            </Card>
          )}
        </>
      )}

      <SpanDetailSheet
        span={selectedSpan}
        open={sheetOpen}
        onOpenChange={setSheetOpen}
        allSpans={traceDetail?.spans}
      />
    </div>
  )
}

// ── AgentActivity ───────────────────────────────────────────────────

export function AgentActivity() {
  const { projects } = useProjects()
  const [selectedProjectId, setSelectedProjectId] = useState<
    number | undefined
  >(projects[0]?.id)
  const [{ range: timeRange, endMs }, selectTimeRange] = useTimeRangeSelection()
  const [systemFilter, setSystemFilter] = useState<string>('')
  const [selectedTraceId, setSelectedTraceId] = useState<string | null>(null)

  const projectId = selectedProjectId ?? projects[0]?.id

  const timeParams = useMemo(() => {
    const to = new Date(endMs).toISOString()
    const from = new Date(endMs - timeRange.hours * 3600_000).toISOString()
    return { from, to }
  }, [endMs, timeRange])

  const { data: tracesResponse, isLoading: tracesLoading } = useQuery({
    queryKey: [
      'genaiTraces',
      projectId,
      timeParams.from,
      timeParams.to,
      systemFilter,
    ],
    queryFn: () =>
      fetchJson<GenAiTraceSummariesResponse>(
        buildOtelUrl('genai/traces', {
          project_id: projectId,
          start_time: timeParams.from,
          end_time: timeParams.to,
          gen_ai_system: systemFilter || undefined,
          limit: 50,
        })
      ),
    enabled: !!projectId,
  })

  const { data: traceDetail, isLoading: detailLoading } = useQuery({
    queryKey: ['genaiTraceDetail', projectId, selectedTraceId],
    queryFn: () =>
      fetchJson<GenAiTraceDetailResponse>(
        buildOtelUrl(`genai/traces/${projectId}/${selectedTraceId}`, {})
      ),
    enabled: !!projectId && !!selectedTraceId,
  })

  const traces = tracesResponse?.data ?? []
  const events = traceDetail?.events ?? []

  // Trace detail view
  if (selectedTraceId && projectId) {
    return (
      <TraceDetailView
        traceId={selectedTraceId}
        traceDetail={traceDetail}
        isLoading={detailLoading}
        events={events}
        onBack={() => setSelectedTraceId(null)}
      />
    )
  }

  return (
    <div className="space-y-4">
      {/* Filters */}
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <h3 className="text-lg font-semibold">Agent Activity</h3>
        <div className="flex flex-col gap-2 sm:flex-row sm:items-center">
          {projects.length > 0 && (
            <Select
              value={projectId?.toString() ?? ''}
              onValueChange={(v) => setSelectedProjectId(Number(v))}
            >
              <SelectTrigger className="w-full sm:w-[180px]">
                <SelectValue placeholder="Select project" />
              </SelectTrigger>
              <SelectContent>
                {projects.map((p) => (
                  <SelectItem key={p.id} value={p.id.toString()}>
                    {p.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          )}
          <Select
            value={systemFilter || 'all'}
            onValueChange={(v) => setSystemFilter(v === 'all' ? '' : v)}
          >
            <SelectTrigger className="w-full sm:w-[150px]">
              <SelectValue placeholder="All providers" />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="all">All providers</SelectItem>
              <SelectItem value="openai">OpenAI</SelectItem>
              <SelectItem value="anthropic">Anthropic</SelectItem>
              <SelectItem value="xai">xAI</SelectItem>
              <SelectItem value="gemini">Google Gemini</SelectItem>
              <SelectItem value="mistral">Mistral</SelectItem>
              <SelectItem value="deepseek">DeepSeek</SelectItem>
            </SelectContent>
          </Select>
          <div className="flex gap-1">
            {TIME_RANGES.map((range) => (
              <Button
                key={range.label}
                variant={
                  timeRange.label === range.label ? 'default' : 'outline'
                }
                size="sm"
                onClick={() => selectTimeRange(range)}
              >
                {range.label}
              </Button>
            ))}
          </div>
        </div>
      </div>

      {/* Traces list */}
      {!projectId ? (
        <EmptyState
          icon={Bot}
          title="No project selected"
          description="Select a project to view AI agent activity traces."
        />
      ) : tracesLoading ? (
        <div className="space-y-2">
          <Skeleton className="h-16 w-full" />
          <Skeleton className="h-16 w-full" />
          <Skeleton className="h-16 w-full" />
        </div>
      ) : traces.length === 0 ? (
        <EmptyState
          icon={Bot}
          title="No AI traces found"
          description="Client applications need to emit OTel spans with gen_ai.* semantic conventions. Point your OTEL_EXPORTER_OTLP_ENDPOINT to this instance."
        />
      ) : (
        <Card>
          <CardContent className="p-0">
            <div className="overflow-x-auto">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>Trace</TableHead>
                    <TableHead>Service</TableHead>
                    <TableHead>Provider</TableHead>
                    <TableHead>Model</TableHead>
                    <TableHead className="hidden md:table-cell">
                      Spans
                    </TableHead>
                    <TableHead className="hidden md:table-cell">
                      Duration
                    </TableHead>
                    <TableHead className="hidden lg:table-cell">
                      Input
                    </TableHead>
                    <TableHead className="hidden lg:table-cell">
                      Output
                    </TableHead>
                    <TableHead className="hidden xl:table-cell">
                      Cache
                    </TableHead>
                    <TableHead>Status</TableHead>
                    <TableHead className="w-8"></TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {traces.map((trace) => {
                    const cachePct = cacheHitPct(trace)
                    return (
                      <TableRow
                        key={trace.trace_id}
                        className="cursor-pointer hover:bg-muted/50"
                        onClick={() => setSelectedTraceId(trace.trace_id)}
                      >
                        <TableCell>
                          <div>
                            <span className="font-mono text-xs block">
                              {trace.root_span_name}
                            </span>
                            <div className="flex items-center gap-1">
                              <span className="text-[10px] text-muted-foreground">
                                {new Date(trace.start_time).toLocaleString([], {
                                  month: 'short',
                                  day: 'numeric',
                                  hour: '2-digit',
                                  minute: '2-digit',
                                })}
                              </span>
                              {trace.gen_ai_operation && (
                                <Badge
                                  variant="outline"
                                  className="text-[9px] px-1 py-0 h-3.5"
                                >
                                  {trace.gen_ai_operation}
                                </Badge>
                              )}
                            </div>
                          </div>
                        </TableCell>
                        <TableCell className="text-xs text-muted-foreground">
                          {trace.service_name}
                        </TableCell>
                        <TableCell>
                          {trace.gen_ai_system ? (
                            <Badge variant="outline" className="text-[10px]">
                              {trace.gen_ai_system}
                            </Badge>
                          ) : (
                            '—'
                          )}
                        </TableCell>
                        <TableCell className="font-mono text-xs">
                          {trace.gen_ai_model ?? '—'}
                        </TableCell>
                        <TableCell className="hidden md:table-cell text-muted-foreground">
                          {trace.span_count}
                        </TableCell>
                        <TableCell className="hidden md:table-cell text-muted-foreground">
                          {trace.duration_ms >= 1000
                            ? `${(trace.duration_ms / 1000).toFixed(1)}s`
                            : `${trace.duration_ms.toFixed(0)}ms`}
                        </TableCell>
                        <TableCell className="hidden lg:table-cell text-muted-foreground tabular-nums">
                          {trace.total_input_tokens != null
                            ? formatTokenCount(trace.total_input_tokens)
                            : '—'}
                        </TableCell>
                        <TableCell className="hidden lg:table-cell text-muted-foreground tabular-nums">
                          {trace.total_output_tokens != null
                            ? formatTokenCount(trace.total_output_tokens)
                            : '—'}
                        </TableCell>
                        <TableCell className="hidden xl:table-cell">
                          {cachePct != null ? (
                            <TooltipProvider>
                              <Tooltip>
                                <TooltipTrigger>
                                  <Badge
                                    variant="outline"
                                    className="text-[10px] text-blue-500 border-blue-500/30"
                                  >
                                    {cachePct}% hit
                                  </Badge>
                                </TooltipTrigger>
                                <TooltipContent>
                                  <p>
                                    {formatTokenCount(
                                      trace.total_cache_read_input_tokens ?? 0
                                    )}{' '}
                                    tokens served from cache
                                  </p>
                                  {trace.total_cache_creation_input_tokens !=
                                    null &&
                                    trace.total_cache_creation_input_tokens >
                                      0 && (
                                      <p>
                                        {formatTokenCount(
                                          trace.total_cache_creation_input_tokens
                                        )}{' '}
                                        tokens written to cache
                                      </p>
                                    )}
                                </TooltipContent>
                              </Tooltip>
                            </TooltipProvider>
                          ) : (
                            <span className="text-muted-foreground">—</span>
                          )}
                        </TableCell>
                        <TableCell>
                          {trace.error_count > 0 ? (
                            <Badge variant="destructive">
                              {trace.error_count} err
                            </Badge>
                          ) : (
                            <Badge
                              variant="default"
                              className="bg-green-500/15 text-green-500 hover:bg-green-500/25"
                            >
                              OK
                            </Badge>
                          )}
                        </TableCell>
                        <TableCell>
                          <ChevronRight className="h-4 w-4 text-muted-foreground" />
                        </TableCell>
                      </TableRow>
                    )
                  })}
                </TableBody>
              </Table>
            </div>
          </CardContent>
        </Card>
      )}

      {tracesResponse && tracesResponse.total > traces.length && (
        <p className="text-sm text-muted-foreground text-center">
          Showing {traces.length} of {tracesResponse.total} traces
        </p>
      )}
    </div>
  )
}

// ── ProjectAgentActivity (exported for project detail page) ─────────

export function ProjectAgentActivity({ projectId }: { projectId: number }) {
  const [{ range: timeRange, endMs }, selectTimeRange] = useTimeRangeSelection()
  const [systemFilter, setSystemFilter] = useState<string>('')
  const [searchParams, setSearchParams] = useSearchParams()
  const selectedTraceId = searchParams.get('trace') || null

  const setSelectedTraceId = (traceId: string | null) => {
    if (traceId) {
      setSearchParams({ trace: traceId })
    } else {
      setSearchParams({})
    }
  }

  const timeParams = useMemo(() => {
    const to = new Date(endMs).toISOString()
    const from = new Date(endMs - timeRange.hours * 3600_000).toISOString()
    return { from, to }
  }, [endMs, timeRange])

  const { data: tracesResponse, isLoading: tracesLoading } = useQuery({
    queryKey: [
      'genaiTraces',
      projectId,
      timeParams.from,
      timeParams.to,
      systemFilter,
    ],
    queryFn: () =>
      fetchJson<GenAiTraceSummariesResponse>(
        buildOtelUrl('genai/traces', {
          project_id: projectId,
          start_time: timeParams.from,
          end_time: timeParams.to,
          gen_ai_system: systemFilter || undefined,
          limit: 50,
        })
      ),
    enabled: !!projectId,
  })

  const { data: traceDetail, isLoading: detailLoading } = useQuery({
    queryKey: ['genaiTraceDetail', projectId, selectedTraceId],
    queryFn: () =>
      fetchJson<GenAiTraceDetailResponse>(
        buildOtelUrl(`genai/traces/${projectId}/${selectedTraceId}`, {})
      ),
    enabled: !!projectId && !!selectedTraceId,
  })

  const traces = tracesResponse?.data ?? []
  const events = traceDetail?.events ?? []

  if (selectedTraceId) {
    return (
      <TraceDetailView
        traceId={selectedTraceId}
        traceDetail={traceDetail}
        isLoading={detailLoading}
        events={events}
        onBack={() => setSelectedTraceId(null)}
      />
    )
  }

  return (
    <div className="space-y-4">
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <h3 className="text-lg font-semibold">AI Activity</h3>
        <div className="flex flex-col gap-2 sm:flex-row sm:items-center">
          <Select
            value={systemFilter || 'all'}
            onValueChange={(v) => setSystemFilter(v === 'all' ? '' : v)}
          >
            <SelectTrigger className="w-full sm:w-[150px]">
              <SelectValue placeholder="All providers" />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="all">All providers</SelectItem>
              <SelectItem value="openai">OpenAI</SelectItem>
              <SelectItem value="anthropic">Anthropic</SelectItem>
              <SelectItem value="xai">xAI</SelectItem>
              <SelectItem value="gemini">Google Gemini</SelectItem>
              <SelectItem value="mistral">Mistral</SelectItem>
              <SelectItem value="deepseek">DeepSeek</SelectItem>
            </SelectContent>
          </Select>
          <div className="flex gap-1">
            {TIME_RANGES.map((range) => (
              <Button
                key={range.label}
                variant={
                  timeRange.label === range.label ? 'default' : 'outline'
                }
                size="sm"
                onClick={() => selectTimeRange(range)}
              >
                {range.label}
              </Button>
            ))}
          </div>
        </div>
      </div>

      {tracesLoading ? (
        <div className="space-y-2">
          <Skeleton className="h-16 w-full" />
          <Skeleton className="h-16 w-full" />
          <Skeleton className="h-16 w-full" />
        </div>
      ) : traces.length === 0 ? (
        <EmptyState
          icon={Bot}
          title="No AI traces found"
          description="Applications need to emit OTel spans with gen_ai.* semantic conventions. Point your OTEL_EXPORTER_OTLP_ENDPOINT to this instance."
        />
      ) : (
        <Card>
          <CardContent className="p-0">
            <div className="overflow-x-auto">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>Trace</TableHead>
                    <TableHead>Service</TableHead>
                    <TableHead>Provider</TableHead>
                    <TableHead>Model</TableHead>
                    <TableHead className="hidden md:table-cell">
                      Spans
                    </TableHead>
                    <TableHead className="hidden md:table-cell">
                      Duration
                    </TableHead>
                    <TableHead className="hidden lg:table-cell">
                      Input
                    </TableHead>
                    <TableHead className="hidden lg:table-cell">
                      Output
                    </TableHead>
                    <TableHead className="hidden xl:table-cell">
                      Cache
                    </TableHead>
                    <TableHead>Status</TableHead>
                    <TableHead className="w-8"></TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {traces.map((trace) => {
                    const cachePct = cacheHitPct(trace)
                    return (
                      <TableRow
                        key={trace.trace_id}
                        className="cursor-pointer hover:bg-muted/50"
                        onClick={() => setSelectedTraceId(trace.trace_id)}
                      >
                        <TableCell>
                          <div>
                            <span className="font-mono text-xs block">
                              {trace.root_span_name}
                            </span>
                            <div className="flex items-center gap-1">
                              <span className="text-[10px] text-muted-foreground">
                                {new Date(trace.start_time).toLocaleString([], {
                                  month: 'short',
                                  day: 'numeric',
                                  hour: '2-digit',
                                  minute: '2-digit',
                                })}
                              </span>
                              {trace.gen_ai_operation && (
                                <Badge
                                  variant="outline"
                                  className="text-[9px] px-1 py-0 h-3.5"
                                >
                                  {trace.gen_ai_operation}
                                </Badge>
                              )}
                            </div>
                          </div>
                        </TableCell>
                        <TableCell className="text-xs text-muted-foreground">
                          {trace.service_name}
                        </TableCell>
                        <TableCell>
                          {trace.gen_ai_system ? (
                            <Badge variant="outline" className="text-[10px]">
                              {trace.gen_ai_system}
                            </Badge>
                          ) : (
                            '—'
                          )}
                        </TableCell>
                        <TableCell className="font-mono text-xs">
                          {trace.gen_ai_model ?? '—'}
                        </TableCell>
                        <TableCell className="hidden md:table-cell text-muted-foreground">
                          {trace.span_count}
                        </TableCell>
                        <TableCell className="hidden md:table-cell text-muted-foreground">
                          {trace.duration_ms >= 1000
                            ? `${(trace.duration_ms / 1000).toFixed(1)}s`
                            : `${trace.duration_ms.toFixed(0)}ms`}
                        </TableCell>
                        <TableCell className="hidden lg:table-cell text-muted-foreground tabular-nums">
                          {trace.total_input_tokens != null
                            ? formatTokenCount(trace.total_input_tokens)
                            : '—'}
                        </TableCell>
                        <TableCell className="hidden lg:table-cell text-muted-foreground tabular-nums">
                          {trace.total_output_tokens != null
                            ? formatTokenCount(trace.total_output_tokens)
                            : '—'}
                        </TableCell>
                        <TableCell className="hidden xl:table-cell">
                          {cachePct != null ? (
                            <TooltipProvider>
                              <Tooltip>
                                <TooltipTrigger>
                                  <Badge
                                    variant="outline"
                                    className="text-[10px] text-blue-500 border-blue-500/30"
                                  >
                                    {cachePct}% hit
                                  </Badge>
                                </TooltipTrigger>
                                <TooltipContent>
                                  <p>
                                    {formatTokenCount(
                                      trace.total_cache_read_input_tokens ?? 0
                                    )}{' '}
                                    tokens served from cache
                                  </p>
                                  {trace.total_cache_creation_input_tokens !=
                                    null &&
                                    trace.total_cache_creation_input_tokens >
                                      0 && (
                                      <p>
                                        {formatTokenCount(
                                          trace.total_cache_creation_input_tokens
                                        )}{' '}
                                        tokens written to cache
                                      </p>
                                    )}
                                </TooltipContent>
                              </Tooltip>
                            </TooltipProvider>
                          ) : (
                            <span className="text-muted-foreground">—</span>
                          )}
                        </TableCell>
                        <TableCell>
                          {trace.error_count > 0 ? (
                            <Badge variant="destructive">
                              {trace.error_count} err
                            </Badge>
                          ) : (
                            <Badge
                              variant="default"
                              className="bg-green-500/15 text-green-500 hover:bg-green-500/25"
                            >
                              OK
                            </Badge>
                          )}
                        </TableCell>
                        <TableCell>
                          <ChevronRight className="h-4 w-4 text-muted-foreground" />
                        </TableCell>
                      </TableRow>
                    )
                  })}
                </TableBody>
              </Table>
            </div>
          </CardContent>
        </Card>
      )}

      {tracesResponse && tracesResponse.total > traces.length && (
        <p className="text-sm text-muted-foreground text-center">
          Showing {traces.length} of {tracesResponse.total} traces
        </p>
      )}
    </div>
  )
}

function DeleteConfirmDialog({
  open,
  onOpenChange,
  providerKey,
  onConfirm,
  isPending,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  providerKey: ProviderKeyResponse | null
  onConfirm: (id: number) => void
  isPending: boolean
}) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Delete Provider Key</DialogTitle>
          <DialogDescription>
            Are you sure you want to delete &quot;{providerKey?.display_name}
            &quot;? This will stop routing requests through this provider key.
          </DialogDescription>
        </DialogHeader>
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            Cancel
          </Button>
          <Button
            variant="destructive"
            onClick={() => providerKey && onConfirm(providerKey.id)}
            disabled={isPending}
          >
            {isPending && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
            Delete
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

// ============================================================================
// Agent CLI credential dialog body (ADR-037) — the CLI-provider counterpart
// to the BYOK "Add Provider Key" form above. Same dialog shell, different
// fields: an auth-flavor picker (a CLI can accept a subscription OAuth
// token, a plain API key, or a config file, depending on provider) plus one
// credential blob, instead of display-name/key/base-url.
// ============================================================================

interface CliCredentialDialogBodyProps {
  cli: ProviderCatalogDto
  authType: string
  onAuthTypeChange: (id: string) => void
  credential: string
  onCredentialChange: (value: string) => void
  onSave: () => void
  saving: boolean
}

function CliCredentialDialogBody({
  cli,
  authType,
  onAuthTypeChange,
  credential,
  onCredentialChange,
  onSave,
  saving,
}: CliCredentialDialogBodyProps) {
  const selectedFlavor =
    cli.auth_flavors.find((f) => f.id === authType) ?? cli.auth_flavors[0]

  return (
    <>
      <DialogHeader>
        <DialogTitle>Configure {cli.name}</DialogTitle>
        <DialogDescription>
          Your credential is encrypted at rest and injected into each agent
          session on this host — not used for gateway BYOK routing.
        </DialogDescription>
      </DialogHeader>
      <div className="grid gap-4 py-4">
        <div className="flex items-center gap-3 rounded-md border bg-muted/30 px-3 py-2.5">
          <AiProviderIcon provider={cli.id} size={36} />
          <div className="min-w-0 flex-1 space-y-0.5">
            <div className="text-sm font-medium">{cli.name}</div>
            <div className="text-xs text-muted-foreground truncate">
              Install:{' '}
              <code className="bg-muted px-1 rounded">
                {cli.install_command}
              </code>
            </div>
            <div className="text-xs text-muted-foreground truncate">
              Auth:{' '}
              <code className="bg-muted px-1 rounded">{cli.auth_command}</code>
            </div>
          </div>
        </div>

        {cli.auth_flavors.length > 1 && (
          <div className="grid grid-cols-1 sm:grid-cols-2 gap-2">
            {cli.auth_flavors.map((flavor) => (
              <button
                key={flavor.id}
                type="button"
                onClick={() => onAuthTypeChange(flavor.id)}
                className={`rounded-md border p-2.5 text-left transition-colors ${
                  selectedFlavor?.id === flavor.id
                    ? 'border-primary bg-primary/10'
                    : 'border-border hover:border-primary/50'
                }`}
              >
                <p className="text-xs font-medium">{flavor.label}</p>
                <p className="text-[11px] text-muted-foreground mt-0.5">
                  {flavor.description}
                </p>
              </button>
            ))}
          </div>
        )}

        <div className="grid gap-2">
          <Label htmlFor="cliCredential">
            {selectedFlavor?.label ?? 'Credential'}
            {selectedFlavor?.env_var && (
              <span className="ml-2 text-xs font-normal text-muted-foreground">
                → injected as {selectedFlavor.env_var}
              </span>
            )}
          </Label>
          {selectedFlavor?.description && (
            <p className="text-xs text-muted-foreground">
              {selectedFlavor.description}
            </p>
          )}
          {selectedFlavor?.format === 'config_file' ? (
            <Textarea
              id="cliCredential"
              placeholder={
                cli.credential_saved
                  ? '••••••••••••• (saved — paste a new file body to replace)'
                  : 'Paste the full file contents here...'
              }
              value={credential}
              onChange={(e) => onCredentialChange(e.target.value)}
              className="min-h-[140px] font-mono text-xs"
            />
          ) : (
            <Input
              id="cliCredential"
              type="password"
              placeholder={
                cli.credential_saved
                  ? '••••••••••••• (saved — paste a new value to replace)'
                  : selectedFlavor?.format === 'oauth_token'
                    ? 'Paste OAuth token...'
                    : 'Paste API key...'
              }
              value={credential}
              onChange={(e) => onCredentialChange(e.target.value)}
            />
          )}
        </div>
      </div>
      <DialogFooter>
        <Button
          onClick={onSave}
          disabled={saving || !credential.trim()}
          className="w-full sm:w-auto"
        >
          {saving ? (
            <>
              <Loader2 className="mr-2 h-4 w-4 animate-spin" />
              Saving…
            </>
          ) : (
            'Save credential'
          )}
        </Button>
      </DialogFooter>
    </>
  )
}

export function AiGatewayPage() {
  const queryClient = useQueryClient()
  const { data: settings } = useSettings()

  usePageTitle('AI Gateway')

  const [dialogOpen, setDialogOpen] = useState(false)
  const [deleteDialogOpen, setDeleteDialogOpen] = useState(false)
  const [selectedKey, setSelectedKey] = useState<ProviderKeyResponse | null>(
    null
  )
  const [snippetLang, setSnippetLang] = useState<
    'bash' | 'python' | 'typescript'
  >('bash')
  const [snippetProvider, setSnippetProvider] = useState<string>('')

  // Form state
  const [newProvider, setNewProvider] = useState('')
  const [newDisplayName, setNewDisplayName] = useState('')
  const [newApiKey, setNewApiKey] = useState('')
  const [newBaseUrl, setNewBaseUrl] = useState('')

  // Agent CLI credential form state — same dialog shell as BYOK, different
  // fields (auth flavor + one credential blob instead of key/name/base-url).
  const [newCliAuthType, setNewCliAuthType] = useState('')
  const [newCliCredential, setNewCliCredential] = useState('')

  // Derive the gateway endpoint from platform settings
  const externalUrl = settings?.external_url || window.location.origin
  const gatewayEndpoint = `${externalUrl}/api/ai/v1`

  // Fetch provider keys
  const { data: keysData, isLoading } = useQuery({
    queryKey: ['providerKeys'],
    queryFn: async () => {
      const response = await listProviderKeys()
      return response.data
    },
  })

  const keys = keysData ?? []

  // Agent CLI catalog (Claude Code, Codex, OpenCode) — rendered alongside
  // the BYOK providers below so switching to a subscription-backed CLI is
  // one list, not a separate settings surface (ADR-037 Phase 2).
  const { data: cliCatalog, isLoading: cliCatalogLoading } = useQuery(
    listAiProvidersOptions()
  )
  const cliProviders = cliCatalog?.providers ?? []
  // Static shell so the three subscription CLI rows render immediately
  // instead of popping in 2-3s late — that delay comes from the backend
  // spawning a subprocess per CLI to check host auth status. Only the
  // Status/Actions cells (which depend on that check) show a skeleton;
  // provider name/description are known ahead of time.
  const { data: providerPreference } = useQuery(getAiProviderStatusOptions())
  const summaryStatus = providerPreference as
    | (typeof providerPreference & {
        available_providers: SummaryProviderOption[]
        summary_preference?: {
          provider_id?: string | null
          model?: string | null
          thinking_level?: string | null
        }
      })
    | undefined

  const activeCliProviderId =
    providerPreference?.active_provider_type === 'agent_cli'
      ? providerPreference.agent_cli_provider_id
      : null

  const preferenceMutation = useMutation({
    ...updateAiProviderPreferenceMutation(),
    meta: { errorTitle: 'Failed to update AI provider preference' },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: getAiProviderStatusQueryKey() })
    },
  })

  const refreshProviderStatusMutation = useMutation({
    ...refreshAiProviderStatusMutation(),
    meta: { errorTitle: 'Failed to refresh AI provider status' },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: getAiProviderStatusQueryKey() })
      queryClient.invalidateQueries({ queryKey: listAiProvidersQueryKey() })
      toast.success('Subscription provider authentication refreshed')
    },
  })

  const refreshAllGatewayModelsMutation = useMutation({
    mutationFn: () =>
      Promise.all(
        keys.map((key) =>
          refreshProviderModels({
            path: { id: key.id },
            throwOnError: true,
          })
        )
      ),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['providerModelInventory'] })
      toast.success('Gateway model catalogs refreshed')
    },
    onError: (error) =>
      toast.error('Some gateway model catalogs could not be refreshed', {
        description: apiErrorMessage(error),
      }),
  })

  const activateCliProvider = (providerId: string, providerName: string) => {
    preferenceMutation.mutate(
      {
        body: { provider_type: 'agent_cli', agent_cli_provider_id: providerId },
      },
      {
        onSuccess: () => {
          toast.success(`Routing AI workloads through ${providerName}`)
        },
      }
    )
  }

  const switchToGateway = () => {
    preferenceMutation.mutate(
      { body: { provider_type: 'gateway', agent_cli_provider_id: null } },
      {
        onSuccess: () => {
          toast.success('Routing AI workloads through the gateway (BYOK)')
        },
      }
    )
  }

  // Save (or replace) a CLI's credential — same encrypted storage the
  // /agent-sandbox/providers detail page uses, just surfaced inline here so
  // "Configure" behaves like the BYOK dialog instead of navigating away.
  const saveCliCredentialMutation = useMutation({
    mutationFn: (vars: {
      providerId: string
      authType: string
      credential: string
    }) =>
      saveAiProviderCredential({
        path: { provider_id: vars.providerId },
        body: { auth_type: vars.authType, credential: vars.credential },
      }),
    meta: { errorTitle: 'Failed to save credential' },
    onSuccess: (_data, vars) => {
      queryClient.invalidateQueries({ queryKey: listAiProvidersQueryKey() })
      setDialogOpen(false)
      resetForm()
      toast.success(
        `${cliProviders.find((p) => p.id === vars.providerId)?.name ?? vars.providerId} credential saved`
      )
    },
  })

  const handleSaveCliCredential = () => {
    if (!newProvider) return
    if (!newCliCredential.trim()) {
      toast.error('Please paste a credential')
      return
    }
    saveCliCredentialMutation.mutate({
      providerId: newProvider,
      authType: newCliAuthType,
      credential: newCliCredential.trim(),
    })
  }

  const openCliDialog = (cli: ProviderCatalogDto) => {
    setNewProvider(cli.id)
    setNewCliAuthType(cli.current_auth_type ?? cli.auth_flavors[0]?.id ?? '')
    setNewCliCredential('')
    setDialogOpen(true)
  }

  // Create mutation
  const createMutation = useMutation({
    mutationFn: (data: {
      provider: string
      display_name: string
      api_key: string
      base_url?: string
    }) => createProviderKey({ body: data }),
    meta: { errorTitle: 'Failed to create provider key' },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['providerKeys'] })
      setDialogOpen(false)
      resetForm()
      toast.success('Provider key added')
    },
  })

  // Delete mutation
  const deleteMutation = useMutation({
    mutationFn: (id: number) => deleteProviderKey({ path: { id } }),
    meta: { errorTitle: 'Failed to delete provider key' },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['providerKeys'] })
      setDeleteDialogOpen(false)
      setSelectedKey(null)
      toast.success('Provider key deleted')
    },
  })

  // Toggle active mutation
  const toggleMutation = useMutation({
    mutationFn: ({ id, is_active }: { id: number; is_active: boolean }) =>
      updateProviderKey({ path: { id }, body: { is_active } }),
    meta: { errorTitle: 'Failed to update provider key' },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['providerKeys'] })
    },
  })

  // Test existing key by ID
  const [testingKeyId, setTestingKeyId] = useState<number | null>(null)
  // Which provider row is expanded in the keys table
  const [expandedProvider, setExpandedProvider] = useState<string | null>(null)
  const testKeyMutation = useMutation({
    mutationFn: async (id: number) => {
      setTestingKeyId(id)
      const response = await testProviderKeyById({
        path: { id },
        throwOnError: true,
      })
      return response.data
    },
    onSuccess: (data) => {
      if (data.success) {
        toast.success(
          `${providerName(data.provider)} key is valid (${data.latency_ms}ms)`
        )
      } else {
        toast.error(`${providerName(data.provider)} key test failed`, {
          description: data.error ?? undefined,
        })
      }
      setTestingKeyId(null)
    },
    onError: (err) => {
      toast.error('Failed to test provider key', {
        description: apiErrorMessage(err),
      })
      setTestingKeyId(null)
    },
  })

  // The add-key dialog used to expose a separate "Test Key" action, but the
  // backend now verifies on create (see handlers/providers.rs) — a failed
  // test surfaces as a 400 on the create call itself.

  const resetForm = () => {
    setNewProvider('')
    setNewDisplayName('')
    setNewApiKey('')
    setNewBaseUrl('')
    setNewCliAuthType('')
    setNewCliCredential('')
  }

  const handleCreate = () => {
    if (!newProvider) {
      toast.error('Please select a provider')
      return
    }
    if (!newApiKey.trim()) {
      toast.error('Please enter an API key')
      return
    }
    if (!newDisplayName.trim()) {
      toast.error('Please enter a display name')
      return
    }

    createMutation.mutate({
      provider: newProvider,
      display_name: newDisplayName,
      api_key: newApiKey,
      base_url: newBaseUrl || undefined,
    })
  }

  const handleDelete = (key: ProviderKeyResponse) => {
    setSelectedKey(key)
    setDeleteDialogOpen(true)
  }

  const handleToggle = (key: ProviderKeyResponse) => {
    toggleMutation.mutate({ id: key.id, is_active: !key.is_active })
  }

  // Pick the first configured provider, or fall back to the first in the list
  const firstConfiguredProvider = SUPPORTED_PROVIDERS.find((p) =>
    keys.some((k) => k.provider === p.id && k.is_active)
  )

  // Which dialog body to render — set when a CLI row's "Configure"/"Update
  // credential" action opened the shared dialog (see openCliDialog above).
  const dialogCliProvider = cliProviders.find((p) => p.id === newProvider)
  const effectiveSnippetProvider =
    snippetProvider || firstConfiguredProvider?.id || SUPPORTED_PROVIDERS[0].id
  const snippetModel =
    SUPPORTED_PROVIDERS.find((p) => p.id === effectiveSnippetProvider)
      ?.defaultModel ?? SUPPORTED_PROVIDERS[0].defaultModel

  const codeSnippets = {
    bash: `curl ${gatewayEndpoint}/chat/completions \\
  -H "Authorization: Bearer YOUR_API_KEY" \\
  -H "Content-Type: application/json" \\
  -d '{
    "model": "${snippetModel}",
    "messages": [{"role": "user", "content": "Hello!"}]
  }'`,
    python: `from openai import OpenAI

client = OpenAI(
    base_url="${gatewayEndpoint}",
    api_key="YOUR_API_KEY",
)

response = client.chat.completions.create(
    model="${snippetModel}",
    messages=[{"role": "user", "content": "Hello!"}],
)
print(response.choices[0].message.content)`,
    typescript: `import OpenAI from "openai";

const client = new OpenAI({
  baseURL: "${gatewayEndpoint}",
  apiKey: "YOUR_API_KEY",
});

const response = await client.chat.completions.create({
  model: "${snippetModel}",
  messages: [{ role: "user", content: "Hello!" }],
});
console.log(response.choices[0].message.content);`,
  }

  const snippetExamples: CodeExample[] = [
    { id: 'bash', label: 'cURL', language: 'bash', code: codeSnippets.bash },
    {
      id: 'python',
      label: 'Python',
      language: 'python',
      code: codeSnippets.python,
    },
    {
      id: 'typescript',
      label: 'Node.js',
      language: 'typescript',
      code: codeSnippets.typescript,
    },
  ]

  return (
    <div className="container mx-auto px-4 sm:px-6 py-4 sm:py-6 space-y-4 sm:space-y-6">
      {/* Page Header */}
      <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        <div>
          <h1 className="text-2xl sm:text-3xl font-bold">AI Gateway</h1>
          <p className="text-muted-foreground mt-1 sm:mt-2 text-sm">
            Unified API for multiple AI providers with a single endpoint
          </p>
        </div>
        <Button
          onClick={() => setDialogOpen(true)}
          className="w-full sm:w-auto"
        >
          <Plus className="mr-2 h-4 w-4" />
          Add Provider Key
        </Button>
      </div>

      {/* Quick Start card — mirrors the provider code example from the Settings
          tab so first-time users see how to actually call the gateway without
          having to click through tabs. Layout intentionally echoes Vercel's
          AI Gateway landing panel: explanation on the left, live code preview
          on the right with segmented language tabs and a provider switcher. */}
      <Card className="overflow-hidden p-0">
        <div className="grid md:grid-cols-2 md:items-stretch">
          <div className="flex flex-col justify-between gap-6 p-6">
            <div className="space-y-2">
              <CardTitle className="text-xl">Start using AI Gateway</CardTitle>
              <CardDescription className="text-sm leading-relaxed">
                One OpenAI-compatible endpoint for every configured provider.
                Swap the base URL, keep the SDK you already use.
              </CardDescription>
            </div>
            <div className="space-y-3">
              <div className="flex items-center gap-2">
                <code className="flex-1 truncate rounded-md bg-muted px-3 py-2 text-xs font-mono">
                  {gatewayEndpoint}
                </code>
                <CopyButton value={gatewayEndpoint} className="shrink-0" />
              </div>
              <div className="flex flex-wrap items-center gap-2">
                <Button size="sm" asChild>
                  <Link to="/ai-gateway/setup">View code examples</Link>
                </Button>
                {!firstConfiguredProvider && (
                  <Button
                    size="sm"
                    variant="outline"
                    onClick={() => setDialogOpen(true)}
                  >
                    <Plus className="mr-2 h-4 w-4" />
                    Add a provider key
                  </Button>
                )}
              </div>
            </div>
          </div>

          {/* Right panel — code preview styled to sit flush against the card
              edges (no inner card-in-card). Header row mirrors Vercel's: tabs
              on the left, provider dropdown on the right. */}
          <div className="md:border-l md:border-t-0">
            <CodeTabs
              className="h-full rounded-none border-0 border-t md:border-l md:border-t-0"
              value={snippetLang}
              onValueChange={(id) =>
                setSnippetLang(id as 'bash' | 'python' | 'typescript')
              }
              examples={snippetExamples}
              rightSlot={
                <Select
                  value={effectiveSnippetProvider}
                  onValueChange={setSnippetProvider}
                >
                  <SelectTrigger className="h-7 w-[150px] text-xs focus:ring-0 focus:ring-offset-0">
                    <SelectValue placeholder="Provider" />
                  </SelectTrigger>
                  <SelectContent>
                    {SUPPORTED_PROVIDERS.map((p) => (
                      <SelectItem key={p.id} value={p.id}>
                        <div className="flex items-center gap-2">
                          <AiProviderIcon provider={p.id} size={16} />
                          <span>{p.name}</span>
                        </div>
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              }
            />
          </div>
        </div>
      </Card>

      {summaryStatus && (
        <AiSummaryDefaultsCard
          key={JSON.stringify(summaryStatus.summary_preference ?? {})}
          providers={summaryStatus.available_providers}
          initialProviderId={summaryStatus.summary_preference?.provider_id}
          initialModel={summaryStatus.summary_preference?.model}
          initialThinking={summaryStatus.summary_preference?.thinking_level}
          onSaved={() => {
            queryClient.invalidateQueries({
              queryKey: getAiProviderStatusQueryKey(),
            })
          }}
        />
      )}

      {/* Provider Keys — compact table: one row per supported provider.
          Click the chevron to expand and see individual keys with per-key
          test/toggle/delete actions. Keeps the screen dense so you can see
          all providers at once. */}
      <div className="space-y-4">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <div>
            <h2 className="text-lg font-semibold">AI providers</h2>
            <p className="text-sm text-muted-foreground">
              Gateway keys and subscription CLIs available on this host.
            </p>
          </div>
          <Button
            variant="outline"
            size="sm"
            onClick={() => {
              refreshProviderStatusMutation.mutate({})
              refreshAllGatewayModelsMutation.mutate()
            }}
            disabled={
              refreshProviderStatusMutation.isPending ||
              refreshAllGatewayModelsMutation.isPending
            }
          >
            <RefreshCw
              className={`mr-2 h-4 w-4 ${
                refreshProviderStatusMutation.isPending ||
                refreshAllGatewayModelsMutation.isPending
                  ? 'animate-spin'
                  : ''
              }`}
            />
            Refresh auth &amp; models
          </Button>
        </div>
        {isLoading ? (
          <div className="space-y-2">
            {[1, 2, 3, 4].map((i) => (
              <Skeleton key={i} className="h-14 w-full" />
            ))}
          </div>
        ) : (
          <div className="overflow-hidden rounded-lg border">
            <Table>
              <TableHeader>
                <TableRow className="hover:bg-transparent">
                  <TableHead className="w-[44px]" />
                  <TableHead>Provider</TableHead>
                  <TableHead className="hidden lg:table-cell">Models</TableHead>
                  <TableHead className="hidden sm:table-cell w-[140px]">
                    Status
                  </TableHead>
                  <TableHead className="w-[180px] text-right">
                    Actions
                  </TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {SUPPORTED_PROVIDERS.map((provider) => {
                  const providerKeys = keys.filter(
                    (k) => k.provider === provider.id
                  )
                  const activeKey = providerKeys.find((k) => k.is_active)
                  const hasAnyKey = providerKeys.length > 0
                  const configured = !!activeKey
                  const expanded = expandedProvider === provider.id && hasAnyKey

                  return (
                    <Fragment key={provider.id}>
                      <TableRow
                        className={
                          hasAnyKey ? 'cursor-pointer' : 'hover:bg-transparent'
                        }
                        onClick={
                          hasAnyKey
                            ? () =>
                                setExpandedProvider(
                                  expanded ? null : provider.id
                                )
                            : undefined
                        }
                      >
                        <TableCell className="py-2 pr-0">
                          {hasAnyKey ? (
                            <ChevronRight
                              className={`h-4 w-4 text-muted-foreground transition-transform ${expanded ? 'rotate-90' : ''}`}
                            />
                          ) : null}
                        </TableCell>
                        <TableCell className="py-2">
                          <div className="flex items-center gap-3 min-w-0">
                            <AiProviderIcon provider={provider.id} size={32} />
                            <div className="min-w-0">
                              <div className="flex items-center gap-2">
                                <span className="text-sm font-medium truncate">
                                  {provider.name}
                                </span>
                                {hasAnyKey && (
                                  <Badge
                                    variant="secondary"
                                    className="h-5 px-1.5 text-[10px]"
                                  >
                                    {providerKeys.length}{' '}
                                    {providerKeys.length === 1 ? 'key' : 'keys'}
                                  </Badge>
                                )}
                              </div>
                              <p className="text-xs text-muted-foreground truncate">
                                {provider.tagline}
                              </p>
                            </div>
                          </div>
                        </TableCell>
                        <TableCell className="hidden lg:table-cell py-2 text-xs text-muted-foreground">
                          <span className="line-clamp-1">
                            {provider.models}
                          </span>
                        </TableCell>
                        <TableCell className="hidden sm:table-cell py-2">
                          {configured ? (
                            <Badge className="justify-center whitespace-nowrap bg-green-500/15 text-green-600 dark:text-green-400 hover:bg-green-500/25">
                              Active
                            </Badge>
                          ) : hasAnyKey ? (
                            <Badge
                              variant="secondary"
                              className="justify-center whitespace-nowrap"
                            >
                              Disabled
                            </Badge>
                          ) : (
                            <Badge
                              variant="outline"
                              className="justify-center whitespace-nowrap text-muted-foreground"
                            >
                              Not configured
                            </Badge>
                          )}
                        </TableCell>
                        <TableCell
                          className="py-2 text-right"
                          onClick={(e) => e.stopPropagation()}
                        >
                          <Button
                            variant={hasAnyKey ? 'outline' : 'default'}
                            size="sm"
                            onClick={() => {
                              setNewProvider(provider.id)
                              setNewDisplayName(
                                hasAnyKey
                                  ? `${provider.name} (key ${providerKeys.length + 1})`
                                  : provider.name
                              )
                              setDialogOpen(true)
                            }}
                          >
                            <Plus className="mr-1.5 h-3.5 w-3.5" />
                            {hasAnyKey ? 'Add key' : 'Configure'}
                          </Button>
                        </TableCell>
                      </TableRow>

                      {expanded &&
                        providerKeys.map((key) => (
                          <Fragment key={key.id}>
                            <TableRow className="bg-muted/30 hover:bg-muted/40">
                              <TableCell />
                              <TableCell colSpan={3} className="py-2">
                                <div className="flex items-center gap-2 min-w-0">
                                  <span className="text-sm font-medium truncate">
                                    {key.display_name}
                                  </span>
                                  {!key.is_active && (
                                    <Badge
                                      variant="secondary"
                                      className="h-5 px-1.5 text-[10px]"
                                    >
                                      Disabled
                                    </Badge>
                                  )}
                                  <code className="text-xs text-muted-foreground truncate">
                                    {key.api_key_masked}
                                  </code>
                                </div>
                              </TableCell>
                              <TableCell className="py-2 text-right">
                                <div className="flex items-center justify-end gap-0.5">
                                  <Button
                                    variant="ghost"
                                    size="icon"
                                    className="h-8 w-8"
                                    onClick={() =>
                                      testKeyMutation.mutate(key.id)
                                    }
                                    disabled={testingKeyId === key.id}
                                    title="Test key"
                                  >
                                    {testingKeyId === key.id ? (
                                      <Loader2 className="h-4 w-4 animate-spin" />
                                    ) : (
                                      <Play className="h-4 w-4" />
                                    )}
                                  </Button>
                                  <Button
                                    variant="ghost"
                                    size="icon"
                                    className="h-8 w-8"
                                    onClick={() => handleToggle(key)}
                                    disabled={toggleMutation.isPending}
                                    title={
                                      key.is_active
                                        ? 'Disable key'
                                        : 'Enable key'
                                    }
                                  >
                                    {key.is_active ? (
                                      <Power className="h-4 w-4" />
                                    ) : (
                                      <PowerOff className="h-4 w-4 text-muted-foreground" />
                                    )}
                                  </Button>
                                  <Button
                                    variant="ghost"
                                    size="icon"
                                    className="h-8 w-8"
                                    onClick={() => handleDelete(key)}
                                    title="Delete key"
                                  >
                                    <Trash2 className="h-4 w-4 text-destructive" />
                                  </Button>
                                </div>
                              </TableCell>
                            </TableRow>
                            <TableRow className="bg-muted/20 hover:bg-muted/20">
                              <TableCell />
                              <TableCell colSpan={4} className="pb-4 pt-1">
                                <ProviderKeyModelDetail providerKey={key} />
                              </TableCell>
                            </TableRow>
                          </Fragment>
                        ))}
                    </Fragment>
                  )
                })}

                {/* Subscription-backed agent CLIs (ADR-037 Phase 2) — same
                      row shape as BYOK providers above, but only one CLI can
                      be the active routing target at a time (unlike BYOK
                      keys, which are additive). "Active" means this CLI is
                      what AI Gateway requests currently route through.

                      Host auth status is checked via a per-CLI subprocess
                      spawn server-side, which takes a couple seconds — the
                      rows below render immediately from a static shell so
                      only the Status/Actions cells (the part that's
                      actually waiting on that check) show a skeleton. */}
                {cliCatalogLoading
                  ? AI_CLI_PROVIDER_SHELL.map((shell) => (
                      <TableRow key={shell.id} className="hover:bg-transparent">
                        <TableCell className="py-2 pr-0" />
                        <TableCell className="py-2">
                          <div className="flex items-center gap-3 min-w-0">
                            <AiProviderIcon provider={shell.id} size={32} />
                            <div className="min-w-0">
                              <div className="flex items-center gap-2">
                                <span className="text-sm font-medium truncate">
                                  {shell.name}
                                </span>
                                <Badge
                                  variant="outline"
                                  className="h-5 px-1.5 text-[10px] text-muted-foreground"
                                >
                                  Subscription
                                </Badge>
                              </div>
                              <Skeleton className="h-3 w-48 mt-1" />
                            </div>
                          </div>
                        </TableCell>
                        <TableCell className="hidden lg:table-cell py-2">
                          <Skeleton className="h-3 w-32" />
                        </TableCell>
                        <TableCell className="hidden sm:table-cell py-2">
                          <Skeleton className="h-5 w-24" />
                        </TableCell>
                        <TableCell className="py-2 text-right">
                          <div className="flex items-center justify-end gap-1.5">
                            <Skeleton className="h-8 w-32" />
                            <Skeleton className="h-8 w-8" />
                          </div>
                        </TableCell>
                      </TableRow>
                    ))
                  : cliProviders.map((cli) => {
                      const isActive = activeCliProviderId === cli.id
                      const expanded = expandedProvider === cli.id
                      return (
                        <Fragment key={cli.id}>
                          <TableRow
                            className="cursor-pointer"
                            onClick={() =>
                              setExpandedProvider(expanded ? null : cli.id)
                            }
                          >
                            <TableCell className="py-2 pr-0">
                              <ChevronRight
                                className={`h-4 w-4 text-muted-foreground transition-transform ${expanded ? 'rotate-90' : ''}`}
                              />
                            </TableCell>
                            <TableCell className="py-2">
                              <div className="flex items-center gap-3 min-w-0">
                                <AiProviderIcon provider={cli.id} size={32} />
                                <div className="min-w-0">
                                  <div className="flex items-center gap-2">
                                    <span className="text-sm font-medium truncate">
                                      {cli.name}
                                    </span>
                                    <Badge
                                      variant="outline"
                                      className="h-5 px-1.5 text-[10px] text-muted-foreground"
                                    >
                                      Subscription
                                    </Badge>
                                    {cli.host_authenticated && (
                                      <Badge
                                        variant="secondary"
                                        className="h-5 px-1.5 text-[10px]"
                                      >
                                        Host environment
                                      </Badge>
                                    )}
                                  </div>
                                  <p className="text-xs text-muted-foreground truncate">
                                    {cli.host_authenticated
                                      ? `${hostAuthLabel(cli.host_auth_method)} — available for host-routed AI tasks; sandbox workflows require a separate credential`
                                      : (cli.host_auth_hint ??
                                        'Not authenticated on this host yet')}
                                  </p>
                                </div>
                              </div>
                            </TableCell>
                            <TableCell className="hidden lg:table-cell py-2 text-xs text-muted-foreground">
                              <span className="line-clamp-1">
                                {cli.models.length > 0
                                  ? cli.models.join(', ')
                                  : 'Model selection lives in the CLI’s own config'}
                              </span>
                            </TableCell>
                            <TableCell className="hidden sm:table-cell py-2">
                              {isActive ? (
                                <Badge className="justify-center whitespace-nowrap bg-green-500/15 text-green-600 dark:text-green-400 hover:bg-green-500/25">
                                  Active
                                </Badge>
                              ) : cli.host_authenticated ? (
                                <Badge
                                  variant="secondary"
                                  className="justify-center whitespace-nowrap"
                                >
                                  Configured
                                </Badge>
                              ) : (
                                <Badge
                                  variant="outline"
                                  className="justify-center whitespace-nowrap text-muted-foreground"
                                >
                                  Not configured
                                </Badge>
                              )}
                            </TableCell>
                            <TableCell
                              className="py-2 text-right"
                              onClick={(event) => event.stopPropagation()}
                            >
                              <div className="flex items-center justify-end gap-1.5">
                                {isActive ? (
                                  <Button
                                    variant="outline"
                                    size="sm"
                                    onClick={switchToGateway}
                                    disabled={preferenceMutation.isPending}
                                  >
                                    Switch to Gateway
                                  </Button>
                                ) : cli.host_authenticated ? (
                                  <Button
                                    variant="default"
                                    size="sm"
                                    onClick={() =>
                                      activateCliProvider(cli.id, cli.name)
                                    }
                                    disabled={preferenceMutation.isPending}
                                  >
                                    Use this provider
                                  </Button>
                                ) : (
                                  <Button
                                    variant="outline"
                                    size="sm"
                                    disabled
                                    title={
                                      cli.host_auth_hint ??
                                      'Not authenticated on this host yet'
                                    }
                                  >
                                    Not authenticated
                                  </Button>
                                )}
                                <Button
                                  variant="ghost"
                                  size="icon"
                                  className="h-8 w-8"
                                  title={
                                    cli.credential_saved
                                      ? 'Update sandbox credential (used by the AI Workflows autofixer, not chat)'
                                      : 'Configure sandbox credential (used by the AI Workflows autofixer, not chat)'
                                  }
                                  onClick={() => openCliDialog(cli)}
                                >
                                  <Wrench className="h-4 w-4" />
                                </Button>
                              </div>
                            </TableCell>
                          </TableRow>
                          {expanded && (
                            <TableRow className="bg-muted/20 hover:bg-muted/20">
                              <TableCell />
                              <TableCell colSpan={4} className="pb-4 pt-1">
                                <div className="space-y-2 rounded-md border bg-background p-3">
                                  <div className="text-sm font-medium">
                                    Models available through {cli.name}
                                  </div>
                                  <p className="text-xs text-muted-foreground">
                                    {cli.host_version
                                      ? `CLI ${cli.host_version} · `
                                      : ''}
                                    Catalog source: {cli.model_source}
                                    {cli.models_refreshed_at
                                      ? ` · refreshed ${new Date(cli.models_refreshed_at).toLocaleString()}`
                                      : ''}
                                  </p>
                                  {cli.models.length > 0 ? (
                                    <div className="flex flex-wrap gap-2">
                                      {cli.models.map((model) => (
                                        <Badge key={model} variant="outline">
                                          <code className="text-xs">
                                            {model}
                                          </code>
                                        </Badge>
                                      ))}
                                    </div>
                                  ) : (
                                    <p className="text-sm text-muted-foreground">
                                      This CLI manages model selection in its
                                      own configuration.
                                    </p>
                                  )}
                                </div>
                              </TableCell>
                            </TableRow>
                          )}
                        </Fragment>
                      )
                    })}
              </TableBody>
            </Table>
          </div>
        )}
      </div>

      {/* Add Provider Key Dialog — provider is locked (set by whichever
          card opened the dialog). No Select inside the dialog; the
          provider identity is shown as a header. Branches between the BYOK
          key form and the agent-CLI credential form so "Configure" behaves
          the same way regardless of which row opened it (ADR-037). */}
      <Dialog
        open={dialogOpen}
        onOpenChange={(open) => {
          setDialogOpen(open)
          if (!open) resetForm()
        }}
      >
        <DialogContent>
          {dialogCliProvider ? (
            <CliCredentialDialogBody
              cli={dialogCliProvider}
              authType={newCliAuthType}
              onAuthTypeChange={setNewCliAuthType}
              credential={newCliCredential}
              onCredentialChange={setNewCliCredential}
              onSave={handleSaveCliCredential}
              saving={saveCliCredentialMutation.isPending}
            />
          ) : (
            <>
              <DialogHeader>
                <DialogTitle>
                  {newProvider
                    ? `Configure ${providerName(newProvider)}`
                    : 'Add Provider Key'}
                </DialogTitle>
                <DialogDescription>
                  Your key is encrypted at rest and used only to route requests
                  through the gateway.
                </DialogDescription>
              </DialogHeader>
              <div className="grid gap-4 py-4">
                {newProvider && (
                  <div className="flex items-center gap-3 rounded-md border bg-muted/30 px-3 py-2.5">
                    <AiProviderIcon provider={newProvider} size={36} />
                    <div className="min-w-0 flex-1">
                      <div className="text-sm font-medium">
                        {providerName(newProvider)}
                      </div>
                      <div className="text-xs text-muted-foreground truncate">
                        {providerModels(newProvider)}
                      </div>
                    </div>
                    {getAiProvider(newProvider)?.keyDocsUrl && (
                      <a
                        href={getAiProvider(newProvider)!.keyDocsUrl}
                        target="_blank"
                        rel="noreferrer"
                        className="text-xs text-muted-foreground hover:text-foreground hover:underline shrink-0"
                      >
                        Get key →
                      </a>
                    )}
                  </div>
                )}
                <div className="grid gap-2">
                  <Label htmlFor="displayName">Display Name</Label>
                  <Input
                    id="displayName"
                    placeholder="Production API Key"
                    value={newDisplayName}
                    onChange={(e) => setNewDisplayName(e.target.value)}
                  />
                </div>
                <div className="grid gap-2">
                  <Label htmlFor="apiKey">API Key</Label>
                  <Input
                    id="apiKey"
                    type="password"
                    placeholder="sk-..."
                    value={newApiKey}
                    onChange={(e) => setNewApiKey(e.target.value)}
                  />
                  <p className="text-xs text-muted-foreground">
                    Your key is encrypted at rest and never exposed in the
                    dashboard.
                  </p>
                </div>
                <div className="grid gap-2">
                  <Label htmlFor="baseUrl">
                    Custom Base URL{' '}
                    <span className="text-muted-foreground font-normal">
                      (optional)
                    </span>
                  </Label>
                  <Input
                    id="baseUrl"
                    placeholder="https://api.openai.com/v1"
                    value={newBaseUrl}
                    onChange={(e) => setNewBaseUrl(e.target.value)}
                  />
                  <p className="text-xs text-muted-foreground">
                    Override the default API endpoint for this provider.
                  </p>
                </div>
              </div>
              <p className="text-xs text-muted-foreground">
                We&apos;ll verify the key works before saving — this usually
                takes 1–2 seconds.
              </p>
              <DialogFooter>
                <Button
                  onClick={handleCreate}
                  disabled={createMutation.isPending}
                  className="w-full sm:w-auto"
                >
                  {createMutation.isPending ? (
                    <>
                      <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                      Verifying &amp; saving…
                    </>
                  ) : (
                    'Add key'
                  )}
                </Button>
              </DialogFooter>
            </>
          )}
        </DialogContent>
      </Dialog>

      {/* Delete Confirmation Dialog */}
      <DeleteConfirmDialog
        open={deleteDialogOpen}
        onOpenChange={(open) => {
          setDeleteDialogOpen(open)
          if (!open) setSelectedKey(null)
        }}
        providerKey={selectedKey}
        onConfirm={(id) => deleteMutation.mutate(id)}
        isPending={deleteMutation.isPending}
      />
    </div>
  )
}

// ── GovernanceSettings ──────────────────────────────────────────────
//
// Per-scope (instance/project/environment) model allowlists, RPM limits, and
// monthly cost budgets. Reads/writes GovernanceConfigResponse rows via
// /ai/governance. Listing is gated behind AiGatewayWrite server-side (it
// exposes budget and allowlist details, so it uses the same operator-only
// permission as mutations) -- a 403 here means "no permission", not "not
// found", so it gets its own explanatory state rather than a blank panel.

const GOVERNANCE_SCOPE_KINDS = [
  { value: 'instance', label: 'Instance (global default)' },
  { value: 'project', label: 'Project' },
  { value: 'environment', label: 'Environment' },
] as const

type GovernanceScopeKind = (typeof GOVERNANCE_SCOPE_KINDS)[number]['value']

const governanceFormSchema = z.object({
  restrictModels: z.boolean(),
  selectedModels: z.array(z.string()),
  maxRequestsPerMinute: z
    .string()
    .refine((v) => v === '' || (Number.isInteger(Number(v)) && Number(v) > 0), {
      message: 'Must be a positive whole number',
    }),
  maxCostPerMonth: z
    .string()
    .refine((v) => v === '' || (Number.isFinite(Number(v)) && Number(v) >= 0), {
      message: 'Must be a non-negative dollar amount',
    }),
})

type GovernanceFormValues = z.infer<typeof governanceFormSchema>

/**
 * "instance" | "project:7" | "environment:12" -> a human label using
 * whatever project/environment names are already loaded on this page. Falls
 * back to the raw scope id rather than fabricating a name for data that
 * hasn't been fetched (e.g. an environment override for a project other than
 * the one currently selected).
 */
function governanceScopeLabel(
  scope: string,
  projects: ProjectResponse[],
  environments: EnvironmentResponse[]
): string {
  if (scope === 'instance') return 'Instance (global default)'
  const [kind, idStr] = scope.split(':')
  const id = Number(idStr)
  if (kind === 'project') {
    return projects.find((p) => p.id === id)?.name ?? `Project #${id}`
  }
  if (kind === 'environment') {
    return environments.find((e) => e.id === id)?.name ?? `Environment #${id}`
  }
  if (kind === 'token') return `Deployment token #${id}`
  return scope
}

function governanceMonthStartIso(): string {
  const now = new Date()
  return new Date(
    Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), 1)
  ).toISOString()
}

function isInsufficientPermissionsError(error: unknown): boolean {
  return (
    !!error &&
    typeof error === 'object' &&
    (error as { type?: string }).type ===
      'https://temps.sh/probs/insufficient-permissions'
  )
}

export function GovernanceSettings() {
  const queryClient = useQueryClient()

  const [scopeKind, setScopeKind] = useState<GovernanceScopeKind>('instance')
  const [selectedProjectId, setSelectedProjectId] = useState<
    number | undefined
  >(undefined)
  const [selectedEnvironmentId, setSelectedEnvironmentId] = useState<
    number | undefined
  >(undefined)
  const [modelPickerOpen, setModelPickerOpen] = useState(false)
  const [removeDialogOpen, setRemoveDialogOpen] = useState(false)

  const { data: projectsData, isLoading: projectsLoading } = useQuery(
    getProjectsOptions({ query: { page: 1, per_page: 100 } })
  )
  const projects = useMemo(() => projectsData?.projects ?? [], [projectsData])

  const { data: environmentsData, isLoading: environmentsLoading } = useQuery({
    ...getEnvironmentsOptions({
      path: { project_id: selectedProjectId ?? 0 },
    }),
    enabled: scopeKind === 'environment' && !!selectedProjectId,
  })
  const environments = useMemo(() => environmentsData ?? [], [environmentsData])

  const scope = useMemo(() => {
    if (scopeKind === 'instance') return 'instance'
    if (scopeKind === 'project') {
      return selectedProjectId ? `project:${selectedProjectId}` : undefined
    }
    return selectedEnvironmentId
      ? `environment:${selectedEnvironmentId}`
      : undefined
  }, [scopeKind, selectedProjectId, selectedEnvironmentId])

  const {
    data: configs,
    isLoading: configsLoading,
    isError: configsIsError,
    error: configsError,
  } = useQuery(listGovernanceConfigsOptions())

  const existingConfig = useMemo(
    () => configs?.find((c) => c.scope === scope),
    [configs, scope]
  )

  const { data: pricing } = useQuery({
    ...getPricingOptions(),
    staleTime: 60 * 60 * 1000,
  })
  const allModels = useMemo(() => {
    const set = new Set<string>()
    for (const m of pricing?.models ?? []) set.add(m.model)
    return Array.from(set).sort()
  }, [pricing])

  const form = useForm<GovernanceFormValues>({
    resolver: zodResolver(governanceFormSchema),
    defaultValues: {
      restrictModels: false,
      selectedModels: [],
      maxRequestsPerMinute: '',
      maxCostPerMonth: '',
    },
  })
  const { reset: resetForm, control: formControl } = form
  const restrictModels = useWatch({
    control: formControl,
    name: 'restrictModels',
  })
  const selectedModels = useWatch({
    control: formControl,
    name: 'selectedModels',
  })

  // Re-sync the draft form whenever the selected scope (or its saved config)
  // changes -- switching scopes must not carry over the previous scope's
  // draft values. `reset` only touches react-hook-form's own internal store,
  // not React state, so this doesn't cascade renders the way a raw setState
  // call in an effect would.
  useEffect(() => {
    resetForm({
      restrictModels: existingConfig?.allowed_models != null,
      selectedModels: existingConfig?.allowed_models ?? [],
      maxRequestsPerMinute:
        existingConfig?.max_requests_per_minute != null
          ? String(existingConfig.max_requests_per_minute)
          : '',
      maxCostPerMonth:
        existingConfig?.max_cost_per_month_microcents != null
          ? String(existingConfig.max_cost_per_month_microcents / 100_000_000)
          : '',
    })
  }, [existingConfig, scope, resetForm])

  const monthStart = useMemo(() => governanceMonthStartIso(), [])
  const { data: monthSummary, isLoading: spendLoading } = useQuery({
    queryKey: [
      'aiUsage',
      'summary',
      'governance-month',
      monthStart,
      scopeKind,
      selectedProjectId,
      selectedEnvironmentId,
    ],
    queryFn: () =>
      fetchJson<UsageSummary>(
        buildUsageUrl('summary', {
          from: monthStart,
          to: new Date().toISOString(),
          project_id:
            scopeKind === 'project' || scopeKind === 'environment'
              ? selectedProjectId?.toString()
              : undefined,
          environment_id:
            scopeKind === 'environment'
              ? selectedEnvironmentId?.toString()
              : undefined,
        })
      ),
    enabled: scopeKind === 'instance' || !!scope,
  })

  const upsertMutation = useMutation({
    ...upsertGovernanceConfigMutation(),
    meta: { errorTitle: 'Failed to save governance policy' },
    onSuccess: (_data, variables) => {
      queryClient.invalidateQueries({
        queryKey: listGovernanceConfigsQueryKey(),
      })
      toast.success(`Limits saved for ${variables.path.scope}`)
    },
  })

  const deleteMutation = useMutation({
    ...deleteGovernanceConfigMutation(),
    meta: { errorTitle: 'Failed to remove governance policy' },
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: listGovernanceConfigsQueryKey(),
      })
      toast.success('Limits removed — this scope now has unlimited spend')
      setRemoveDialogOpen(false)
    },
  })

  const onSubmit = (values: GovernanceFormValues) => {
    if (!scope) return
    const maxRequestsPerMinute =
      values.maxRequestsPerMinute === ''
        ? null
        : Math.round(Number(values.maxRequestsPerMinute))
    const maxCostPerMonthMicrocents =
      values.maxCostPerMonth === ''
        ? null
        : Math.round(Number(values.maxCostPerMonth) * 100_000_000)
    upsertMutation.mutate({
      path: { scope },
      body: {
        allowed_models: values.restrictModels ? values.selectedModels : null,
        max_requests_per_minute: maxRequestsPerMinute,
        max_cost_per_month_microcents: maxCostPerMonthMicrocents,
      },
    })
  }

  const toggleModel = (model: string) => {
    const current = form.getValues('selectedModels')
    form.setValue(
      'selectedModels',
      current.includes(model)
        ? current.filter((m) => m !== model)
        : [...current, model],
      { shouldDirty: true }
    )
  }

  if (configsIsError && isInsufficientPermissionsError(configsError)) {
    return (
      <EmptyState
        icon={ShieldCheck}
        title="You don't have access to Governance"
        description="Viewing and configuring AI gateway cost limits requires the AI Gateway management permission. Ask an operator to grant it."
      />
    )
  }

  if (configsIsError) {
    return (
      <EmptyState
        icon={AlertTriangle}
        title="Couldn't load governance settings"
        description={apiErrorMessage(configsError)}
      />
    )
  }

  const scopeDisplayLabel = scope
    ? governanceScopeLabel(scope, projects, environments)
    : null

  const budgetMicrocents = existingConfig?.max_cost_per_month_microcents ?? null
  const spendMicrocents = monthSummary?.total_cost_microcents ?? 0
  const spendDollars = spendMicrocents / 100_000_000
  const budgetDollars =
    budgetMicrocents != null ? budgetMicrocents / 100_000_000 : null
  const spendPct =
    budgetDollars != null && budgetDollars > 0
      ? Math.min(100, (spendDollars / budgetDollars) * 100)
      : null

  return (
    <div className="space-y-4">
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <div>
          <h3 className="text-lg font-semibold">Governance</h3>
          <p className="text-sm text-muted-foreground">
            Set model allowlists, request-rate limits, and monthly spend caps
            per instance, project, or environment.
          </p>
        </div>
      </div>

      <Card>
        <CardHeader>
          <CardTitle className="text-base">Scope</CardTitle>
          <CardDescription>
            Instance limits apply everywhere by default. A project or
            environment override replaces the instance limit for that scope
            only.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <div className="flex flex-col gap-2 sm:flex-row sm:items-center">
            <Select
              value={scopeKind}
              onValueChange={(v) => {
                setScopeKind(v as GovernanceScopeKind)
                setSelectedProjectId(undefined)
                setSelectedEnvironmentId(undefined)
              }}
            >
              <SelectTrigger className="w-full sm:w-[220px]">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {GOVERNANCE_SCOPE_KINDS.map((k) => (
                  <SelectItem key={k.value} value={k.value}>
                    {k.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>

            {(scopeKind === 'project' || scopeKind === 'environment') && (
              <Select
                value={selectedProjectId?.toString() ?? ''}
                onValueChange={(v) => {
                  setSelectedProjectId(Number(v))
                  setSelectedEnvironmentId(undefined)
                }}
                disabled={projectsLoading || projects.length === 0}
              >
                <SelectTrigger className="w-full sm:w-[220px]">
                  <SelectValue
                    placeholder={
                      projectsLoading ? 'Loading projects…' : 'Select project'
                    }
                  />
                </SelectTrigger>
                <SelectContent>
                  {projects.map((p) => (
                    <SelectItem key={p.id} value={p.id.toString()}>
                      {p.name}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            )}

            {scopeKind === 'environment' && (
              <Select
                value={selectedEnvironmentId?.toString() ?? ''}
                onValueChange={(v) => setSelectedEnvironmentId(Number(v))}
                disabled={
                  !selectedProjectId ||
                  environmentsLoading ||
                  environments.length === 0
                }
              >
                <SelectTrigger className="w-full sm:w-[220px]">
                  <SelectValue
                    placeholder={
                      !selectedProjectId
                        ? 'Select a project first'
                        : environmentsLoading
                          ? 'Loading environments…'
                          : 'Select environment'
                    }
                  />
                </SelectTrigger>
                <SelectContent>
                  {environments.map((e) => (
                    <SelectItem key={e.id} value={e.id.toString()}>
                      {e.name}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            )}
          </div>
        </CardContent>
      </Card>

      {!scope ? (
        <EmptyState
          icon={SlidersHorizontal}
          title="Select a scope"
          description="Choose a project or environment above to view or set its AI gateway limits."
        />
      ) : configsLoading ? (
        <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
          <Skeleton className="h-96 w-full rounded-lg" />
          <Skeleton className="h-48 w-full rounded-lg" />
        </div>
      ) : (
        <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
          <Card>
            <CardHeader>
              <CardTitle className="text-base">
                Limits for {scopeDisplayLabel}
              </CardTitle>
              <CardDescription>
                {existingConfig
                  ? 'Custom limits are active for this scope.'
                  : 'No custom limits are set — this scope currently inherits unlimited spend and full model access.'}
              </CardDescription>
            </CardHeader>
            <CardContent>
              <Form {...form}>
                <form
                  noValidate
                  onSubmit={form.handleSubmit(onSubmit)}
                  className="space-y-5"
                >
                  <div className="space-y-2">
                    <div className="flex items-center gap-2">
                      <Checkbox
                        id="restrict-models"
                        checked={restrictModels}
                        onCheckedChange={(checked) =>
                          form.setValue('restrictModels', checked === true, {
                            shouldDirty: true,
                          })
                        }
                      />
                      <Label htmlFor="restrict-models" className="font-normal">
                        Restrict to specific models
                      </Label>
                    </div>
                    <p className="text-xs text-muted-foreground">
                      {restrictModels
                        ? selectedModels.length === 0
                          ? 'No models selected — every request to this scope will be denied.'
                          : `Only the ${selectedModels.length} model${selectedModels.length === 1 ? '' : 's'} selected below may be used.`
                        : 'Unchecked (default): every model from every configured provider is allowed.'}
                    </p>
                    {restrictModels && (
                      <>
                        <Popover
                          open={modelPickerOpen}
                          onOpenChange={setModelPickerOpen}
                        >
                          <PopoverTrigger asChild>
                            <Button
                              type="button"
                              variant="outline"
                              role="combobox"
                              className="w-full justify-between font-normal"
                            >
                              {selectedModels.length === 0
                                ? 'Choose models…'
                                : `${selectedModels.length} model${selectedModels.length === 1 ? '' : 's'} selected`}
                              <ChevronsUpDown className="h-4 w-4 opacity-50" />
                            </Button>
                          </PopoverTrigger>
                          <PopoverContent
                            align="start"
                            className="w-[min(calc(100vw-2rem),320px)] p-0"
                          >
                            <Command>
                              <CommandInput placeholder="Search models…" />
                              <CommandList className="max-h-[280px]">
                                <CommandEmpty>No models found.</CommandEmpty>
                                {allModels.map((model) => {
                                  const isSelected =
                                    selectedModels.includes(model)
                                  return (
                                    <CommandItem
                                      key={model}
                                      value={model}
                                      onSelect={() => toggleModel(model)}
                                      className="gap-2"
                                    >
                                      <Check
                                        className={
                                          isSelected
                                            ? 'h-4 w-4 opacity-100'
                                            : 'h-4 w-4 opacity-0'
                                        }
                                      />
                                      <span className="truncate font-mono text-xs">
                                        {model}
                                      </span>
                                    </CommandItem>
                                  )
                                })}
                              </CommandList>
                            </Command>
                          </PopoverContent>
                        </Popover>
                        {selectedModels.length > 0 && (
                          <div className="flex flex-wrap gap-1">
                            {selectedModels.map((model) => (
                              <Badge
                                key={model}
                                variant="secondary"
                                className="gap-1 font-mono text-[11px]"
                              >
                                {model}
                                <button
                                  type="button"
                                  aria-label={`Remove ${model}`}
                                  onClick={() => toggleModel(model)}
                                  className="hover:text-destructive"
                                >
                                  <X className="h-3 w-3" />
                                </button>
                              </Badge>
                            ))}
                          </div>
                        )}
                      </>
                    )}
                  </div>

                  <FormField
                    control={form.control}
                    name="maxRequestsPerMinute"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Requests per minute</FormLabel>
                        <FormControl>
                          <Input
                            {...field}
                            type="number"
                            min={1}
                            step={1}
                            placeholder="Unlimited"
                          />
                        </FormControl>
                        <p className="text-xs text-muted-foreground">
                          Leave blank for no rate limit.
                        </p>
                        <FormMessage />
                      </FormItem>
                    )}
                  />

                  <FormField
                    control={form.control}
                    name="maxCostPerMonth"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Monthly budget (USD)</FormLabel>
                        <FormControl>
                          <Input
                            {...field}
                            type="number"
                            min={0}
                            step={0.01}
                            placeholder="Unlimited"
                          />
                        </FormControl>
                        <p className="text-xs text-muted-foreground">
                          Requests that would exceed this are rejected for the
                          rest of the calendar month. Leave blank for no budget.
                        </p>
                        <FormMessage />
                      </FormItem>
                    )}
                  />

                  <div className="flex flex-wrap gap-2">
                    <Button type="submit" disabled={upsertMutation.isPending}>
                      {upsertMutation.isPending ? (
                        <Loader2 className="mr-1.5 h-4 w-4 animate-spin" />
                      ) : (
                        <Save className="mr-1.5 h-4 w-4" />
                      )}
                      Save limits
                    </Button>
                    <Button
                      type="button"
                      variant="outline"
                      disabled={!existingConfig || deleteMutation.isPending}
                      onClick={() => setRemoveDialogOpen(true)}
                    >
                      <Trash2 className="mr-1.5 h-4 w-4" />
                      Remove limits
                    </Button>
                  </div>
                </form>
              </Form>
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <CardTitle className="flex items-center gap-2 text-base">
                <DollarSign className="h-4 w-4 text-muted-foreground" />
                Spend this month
              </CardTitle>
              <CardDescription>
                {scopeKind === 'instance'
                  ? 'Total AI gateway spend across the whole instance, this calendar month.'
                  : scope
                    ? `AI gateway spend for this ${scopeKind}, this calendar month.`
                    : `Select a ${scopeKind} above to see its spend.`}
              </CardDescription>
            </CardHeader>
            <CardContent className="space-y-3">
              {spendLoading ? (
                <Skeleton className="h-16 w-full rounded-lg" />
              ) : (
                <>
                  <div className="text-2xl font-semibold">
                    {formatCost(spendDollars)}
                    {budgetDollars != null && (
                      <span className="text-base font-normal text-muted-foreground">
                        {' '}
                        / {formatCost(budgetDollars)}
                      </span>
                    )}
                  </div>
                  {spendPct != null ? (
                    <Progress value={spendPct} />
                  ) : (
                    <p className="text-xs text-muted-foreground">
                      No monthly budget set for this scope — spend is unlimited.
                    </p>
                  )}
                </>
              )}
              <Button variant="link" size="sm" className="h-auto p-0" asChild>
                <Link to="/ai-gateway/usage">View detailed usage →</Link>
              </Button>
            </CardContent>
          </Card>
        </div>
      )}

      <AlertDialog open={removeDialogOpen} onOpenChange={setRemoveDialogOpen}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>
              Remove limits for {scopeDisplayLabel}?
            </AlertDialogTitle>
            <AlertDialogDescription>
              This deletes the custom model allowlist, rate limit, and budget
              for this scope. It reverts to unlimited spend and full model
              access until a new limit is saved.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              onClick={() =>
                scope && deleteMutation.mutate({ path: { scope } })
              }
              disabled={deleteMutation.isPending}
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
            >
              {deleteMutation.isPending ? (
                <Loader2 className="mr-1.5 h-4 w-4 animate-spin" />
              ) : null}
              Remove
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  )
}
