import {
  listMetricLabelKeysOptions,
  listMetricLabelValuesOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Command,
  CommandEmpty,
  CommandInput,
  CommandItem,
  CommandList,
} from '@/components/ui/command'
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover'
import { useQuery } from '@tanstack/react-query'
import {
  AlertTriangle,
  Check,
  ChevronsUpDown,
  Layers,
  Plus,
  Tag,
  X,
} from 'lucide-react'
import { useEffect, useState } from 'react'

// ── Label filters ────────────────────────────────────────────────────────────
//
// Shared by MetricsExplorer (URL-persisted filters on an ad-hoc query),
// MetricAlertForm (scoping an alert rule to a label value), and the dashboard
// tile editor (scoping a tile's chart) — one filter-building UI and
// autocomplete behavior for all three.

export interface LabelFilter {
  key: string
  value: string
}

/** Serialize label filters to a compact, regen-ready URL string: `k=v,k2=v2`. */
export function serializeLabelFilters(filters: LabelFilter[]): string {
  return filters
    .filter((f) => f.key.trim().length > 0)
    .map((f) => `${f.key.trim()}=${f.value.trim()}`)
    .join(',')
}

/**
 * Convert draft `{key,value}` rows into the API's ordered-tuple shape
 * (`CreateMetricAlertRequest`/`UpdateMetricAlertRequest`/`DashboardTile` all
 * carry `label_filters` this way), dropping incomplete rows with an empty key.
 */
export function labelFiltersToTuples(
  filters: LabelFilter[]
): [string, string][] {
  return filters
    .filter((f) => f.key.trim().length > 0)
    .map((f) => [f.key.trim(), f.value.trim()] as [string, string])
}

/** Convert the API's ordered-tuple shape back into draft `{key,value}` rows. */
export function tuplesToLabelFilters(
  tuples: readonly (readonly [string, string])[] | null | undefined
): LabelFilter[] {
  return (tuples ?? []).map(([key, value]) => ({ key, value }))
}

/**
 * Discover the attribute keys observed on a metric — powers both the label
 * filter key autocomplete below and the dashboard tile editor's "break down
 * by" key picker, so both stay on the exact same query shape/cache entry.
 * Disabled without a single metric selected (no attributes to inspect).
 */
export function useMetricLabelKeys({
  projectId,
  metricName,
  fromIso,
  toIso,
}: {
  projectId: number
  metricName: string
  fromIso: string
  toIso: string
}) {
  return useQuery({
    ...listMetricLabelKeysOptions({
      query: {
        project_id: projectId,
        metric_name: metricName,
        start_time: fromIso,
        end_time: toIso,
      },
    }),
    enabled: !!projectId && metricName.length > 0,
  })
}

export function LabelFilterBuilder({
  value,
  onChange,
  projectId,
  metricName,
  fromIso,
  toIso,
}: {
  value: LabelFilter[]
  onChange: (next: LabelFilter[]) => void
  projectId: number
  /** The selected metric whose attributes drive autocomplete; '' in overview. */
  metricName: string
  fromIso: string
  toIso: string
}) {
  // Draft rows live in LOCAL state so an incomplete/empty row can exist while
  // you type. The URL (via onChange → serializeLabelFilters) only ever keeps the
  // COMPLETE pairs, so a freshly-added blank row would otherwise round-trip back
  // out of existence and the "Add" button would appear to do nothing.
  const [rows, setRows] = useState<LabelFilter[]>(value)

  // Re-seed only on an EXTERNAL change (back/forward, shared link) — i.e. when
  // the URL's complete set diverges from ours. Our own edits echo back equal, so
  // the in-progress drafts survive instead of being clobbered.
  const valueKey = serializeLabelFilters(value)
  useEffect(() => {
    if (valueKey !== serializeLabelFilters(rows)) setRows(value)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [valueKey])

  const commit = (next: LabelFilter[]) => {
    setRows(next)
    onChange(next)
  }
  const add = () => commit([...rows, { key: '', value: '' }])
  const update = (i: number, patch: Partial<LabelFilter>) =>
    commit(rows.map((f, idx) => (idx === i ? { ...f, ...patch } : f)))
  const remove = (i: number) => commit(rows.filter((_, idx) => idx !== i))

  const keysQuery = useMetricLabelKeys({ projectId, metricName, fromIso, toIso })
  const allKeys = keysQuery.data?.keys ?? []

  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-1.5 text-xs font-medium text-muted-foreground">
          <Tag className="size-3.5" />
          Label filters
        </div>
        <Button
          type="button"
          variant="ghost"
          size="sm"
          onClick={add}
          className="h-7 gap-1 px-2 text-xs"
        >
          <Plus className="size-3" />
          Add
        </Button>
      </div>
      {rows.length === 0 ? (
        <p className="text-xs text-muted-foreground">
          No label filters. Add key/value pairs to narrow the series by
          attribute (e.g. <span className="font-mono">http.method = GET</span>).
        </p>
      ) : (
        <div className="flex flex-col gap-1.5">
          {rows.map((f, i) => (
            <LabelFilterRow
              // Index key is stable here — rows are only appended/removed, never
              // reordered — so the per-row value query keeps its identity.
              key={i}
              row={f}
              // Suggest only keys not already used by another row.
              availableKeys={allKeys.filter(
                (k) => k === f.key || !rows.some((r) => r.key === k)
              )}
              keysLoading={keysQuery.isFetching}
              projectId={projectId}
              metricName={metricName}
              fromIso={fromIso}
              toIso={toIso}
              onUpdate={(patch) => update(i, patch)}
              onRemove={() => remove(i)}
            />
          ))}
          <p className="text-[11px] text-muted-foreground">
            Keys and values autocomplete from the metric&apos;s observed
            attributes (last 24h). Free text is still accepted for values not in
            the recent sample. Filters are URL-persisted so the view stays
            shareable.
          </p>
        </div>
      )}
    </div>
  )
}

/** One key=value row with autocomplete on both sides. */
export function LabelFilterRow({
  row,
  availableKeys,
  keysLoading,
  projectId,
  metricName,
  fromIso,
  toIso,
  onUpdate,
  onRemove,
}: {
  row: LabelFilter
  availableKeys: string[]
  keysLoading: boolean
  projectId: number
  metricName: string
  fromIso: string
  toIso: string
  onUpdate: (patch: Partial<LabelFilter>) => void
  onRemove: () => void
}) {
  // Values for THIS row's key — fetched only once a key is chosen on a metric.
  const valuesQuery = useQuery({
    ...listMetricLabelValuesOptions({
      query: {
        project_id: projectId,
        metric_name: metricName,
        label_key: row.key,
        start_time: fromIso,
        end_time: toIso,
      },
    }),
    enabled: !!projectId && metricName.length > 0 && row.key.trim().length > 0,
  })

  return (
    <div className="flex items-center gap-1.5">
      <SuggestCombobox
        value={row.key}
        options={availableKeys}
        loading={keysLoading}
        placeholder="label.key"
        searchPlaceholder="Search keys…"
        ariaLabel="Label key"
        widthClass="w-full sm:w-[200px]"
        // Changing the key invalidates the previously chosen value.
        onChange={(next) => onUpdate({ key: next, value: '' })}
      />
      <span className="text-muted-foreground">=</span>
      <SuggestCombobox
        value={row.value}
        options={valuesQuery.data?.values ?? []}
        loading={valuesQuery.isFetching}
        placeholder="value"
        searchPlaceholder="Search values…"
        ariaLabel="Label value"
        widthClass="flex-1"
        disabled={row.key.trim().length === 0}
        onChange={(next) => onUpdate({ value: next })}
      />
      <Button
        type="button"
        variant="ghost"
        size="icon"
        onClick={onRemove}
        className="h-8 w-8 shrink-0"
        aria-label="Remove label filter"
      >
        <X className="size-3.5" />
      </Button>
    </div>
  )
}

/**
 * A combobox that suggests discovered options but still accepts free text — the
 * sampled discovery may miss a rare key/value, so typing anything commits it via
 * the "Use …" affordance (or Enter).
 */
export function SuggestCombobox({
  value,
  options,
  loading,
  placeholder,
  searchPlaceholder,
  ariaLabel,
  widthClass,
  disabled,
  onChange,
}: {
  value: string
  options: string[]
  loading?: boolean
  placeholder: string
  searchPlaceholder: string
  ariaLabel: string
  widthClass?: string
  disabled?: boolean
  onChange: (next: string) => void
}) {
  const [open, setOpen] = useState(false)
  const [query, setQuery] = useState('')
  const typed = query.trim()
  const hasExactOption = options.includes(typed)

  const choose = (next: string) => {
    onChange(next)
    setQuery('')
    setOpen(false)
  }

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <Button
          type="button"
          variant="outline"
          role="combobox"
          aria-expanded={open}
          aria-label={ariaLabel}
          disabled={disabled}
          className={[
            'h-8 justify-between gap-1.5 px-2.5 font-mono text-xs font-normal',
            widthClass ?? '',
            value ? '' : 'text-muted-foreground',
          ].join(' ')}
        >
          <span className="truncate">{value || placeholder}</span>
          <ChevronsUpDown className="size-3.5 shrink-0 opacity-50" />
        </Button>
      </PopoverTrigger>
      <PopoverContent align="start" className="w-[240px] p-0">
        <Command>
          <CommandInput
            value={query}
            onValueChange={setQuery}
            placeholder={searchPlaceholder}
            className="text-xs"
          />
          <CommandList className="max-h-[240px]">
            {loading ? (
              <div className="py-4 text-center text-xs text-muted-foreground">
                Loading…
              </div>
            ) : (
              <>
                {typed && !hasExactOption && (
                  <CommandItem value={typed} onSelect={() => choose(typed)}>
                    <Check className="size-3.5 shrink-0 opacity-0" />
                    <span className="truncate font-mono text-xs">
                      Use “{typed}”
                    </span>
                  </CommandItem>
                )}
                <CommandEmpty>No matches.</CommandEmpty>
                {options.map((o) => (
                  <CommandItem
                    key={o}
                    value={o}
                    onSelect={() => choose(o)}
                    className="gap-2"
                  >
                    <Check
                      className={[
                        'size-3.5 shrink-0',
                        o === value ? 'opacity-100' : 'opacity-0',
                      ].join(' ')}
                    />
                    <span className="truncate font-mono text-xs">{o}</span>
                  </CommandItem>
                ))}
              </>
            )}
          </CommandList>
        </Command>
      </PopoverContent>
    </Popover>
  )
}

// ── Group-by (breakdown) builder ────────────────────────────────────────────
//
// Shared by the dashboard tile editor (Phase 2 breakdown charts) and
// MetricAlertForm (Phase 3/4 per-series "dynamic" alerting) — both let a user
// pick up to 2 label keys to split a metric into one series per distinct
// value combination, so this is one control instead of two copies.

export const MAX_GROUP_BY_KEYS = 2

const DEFAULT_GROUP_BY_CARDINALITY_HINT =
  'this may produce a cluttered chart (only the top 20 series are shown).'

/**
 * "Break down by" control: pick up to 2 label keys to render one line per
 * distinct value combination. Reuses the exact key-lookup hook
 * `LabelFilterBuilder` uses so both stay on the same query/cache entry.
 */
export function GroupByBuilder({
  value,
  onChange,
  projectId,
  metricName,
  fromIso,
  toIso,
  cardinalityHint,
}: {
  value: string[]
  onChange: (next: string[]) => void
  projectId: number
  metricName: string
  fromIso: string
  toIso: string
  /**
   * Tail of the per-key cardinality warning sentence — callers cap the
   * resulting series differently (a dashboard tile at a fixed render limit,
   * an alert rule at its own configurable `max_series`), so the wording isn't
   * shared. Defaults to the dashboard tile's fixed-cap phrasing.
   */
  cardinalityHint?: string
}) {
  const keysQuery = useMetricLabelKeys({ projectId, metricName, fromIso, toIso })
  const availableKeys = (keysQuery.data?.keys ?? []).filter(
    (k) => !value.includes(k),
  )
  const atMax = value.length >= MAX_GROUP_BY_KEYS

  const addKey = (key: string) => {
    const trimmed = key.trim()
    if (!trimmed || atMax || value.includes(trimmed)) return
    onChange([...value, trimmed])
  }
  const removeKey = (key: string) => onChange(value.filter((k) => k !== key))

  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-center gap-1.5 text-xs font-medium text-muted-foreground">
        <Layers className="size-3.5" />
        Break down by
      </div>
      <div className="flex flex-wrap items-center gap-1.5">
        {value.map((key) => (
          <Badge
            key={key}
            variant="secondary"
            className="gap-1 font-mono text-xs"
          >
            {key}
            <button
              type="button"
              onClick={() => removeKey(key)}
              aria-label={`Remove ${key}`}
            >
              <X className="size-3" />
            </button>
          </Badge>
        ))}
        {!atMax && (
          <SuggestCombobox
            value=""
            options={availableKeys}
            loading={keysQuery.isFetching}
            placeholder="Add key…"
            searchPlaceholder="Search keys…"
            ariaLabel="Break-down key"
            widthClass="w-[160px]"
            onChange={addKey}
          />
        )}
      </div>
      {value.length === 0 ? (
        <p className="text-xs text-muted-foreground">
          Render one line per distinct value of up to 2 label keys (e.g.{' '}
          <span className="font-mono">region</span>) instead of a single
          aggregate line.
        </p>
      ) : (
        value.map((key) => (
          <GroupByCardinalityWarning
            key={key}
            projectId={projectId}
            metricName={metricName}
            labelKey={key}
            fromIso={fromIso}
            toIso={toIso}
            hint={cardinalityHint ?? DEFAULT_GROUP_BY_CARDINALITY_HINT}
          />
        ))
      )}
    </div>
  )
}

// Distinct-value count above which a breakdown key is flagged as likely to
// produce a lot of series — matches MetricTile's MAX_BREAKDOWN_SERIES cap for
// the dashboard case, so the default warning fires exactly when series would
// actually start getting dropped there.
const GROUP_BY_CARDINALITY_WARNING_THRESHOLD = 20
// The backend caps this lookup at 500 rows (ClickHouse LIMIT in
// list_metric_label_values), so hitting it means "at least this many".
const GROUP_BY_CARDINALITY_VALUES_CAP = 500

/**
 * Per-key cardinality hint, fetched lazily once a key is added to the
 * breakdown. Its own component (not inlined in a `.map`) so the query hook
 * gets one call per selected key, matching the rules-of-hooks reason
 * `TileEditor` itself was extracted.
 */
function GroupByCardinalityWarning({
  projectId,
  metricName,
  labelKey,
  fromIso,
  toIso,
  hint,
}: {
  projectId: number
  metricName: string
  labelKey: string
  fromIso: string
  toIso: string
  hint: string
}) {
  const valuesQuery = useQuery({
    ...listMetricLabelValuesOptions({
      query: {
        project_id: projectId,
        metric_name: metricName,
        label_key: labelKey,
        start_time: fromIso,
        end_time: toIso,
      },
    }),
    enabled:
      !!projectId && metricName.length > 0 && labelKey.trim().length > 0,
  })

  const count = valuesQuery.data?.values.length ?? 0
  if (count <= GROUP_BY_CARDINALITY_WARNING_THRESHOLD) return null

  const countLabel =
    count >= GROUP_BY_CARDINALITY_VALUES_CAP ? `${count}+` : `${count}`

  return (
    <p className="flex items-center gap-1.5 text-[11px] text-amber-600 dark:text-amber-400">
      <AlertTriangle className="size-3 shrink-0" />
      {countLabel} distinct values for{' '}
      <span className="font-mono">{labelKey}</span> — {hint}
    </p>
  )
}

/** Chips beyond this count collapse into a single "+N" badge — a tile with
 * all 10 allowed label_filters would otherwise wrap across several lines and
 * dominate a compact dashboard card. */
const MAX_VISIBLE_FILTER_CHIPS = 3

/**
 * Compact, wrapping indicator of a chart's active label filters / group-by —
 * so a scoped or broken-down chart reads as scoped at a glance (in a
 * dashboard grid or the explorer), not just inside the edit panel. Renders
 * nothing when there's nothing active.
 */
export function LabelFilterChips({
  filters,
  groupBy,
}: {
  filters: [string, string][]
  groupBy?: string[]
}) {
  if (filters.length === 0 && (!groupBy || groupBy.length === 0)) return null

  const chips = filters.map(([key, value]) => `${key}=${value}`)
  const visible = chips.slice(0, MAX_VISIBLE_FILTER_CHIPS)
  const hidden = chips.slice(MAX_VISIBLE_FILTER_CHIPS)

  return (
    <div className="flex flex-wrap items-center gap-1">
      {visible.map((chip, i) => (
        <Badge
          key={`${chip}-${i}`}
          variant="outline"
          className="max-w-[140px] gap-1 truncate font-mono text-[10px]"
          title={chip}
        >
          <Tag className="size-2.5 shrink-0" />
          <span className="truncate">{chip}</span>
        </Badge>
      ))}
      {hidden.length > 0 && (
        <Badge
          variant="outline"
          className="text-[10px]"
          title={hidden.join(', ')}
        >
          +{hidden.length} more
        </Badge>
      )}
      {groupBy && groupBy.length > 0 && (
        <Badge variant="secondary" className="gap-1 text-[10px]">
          <Layers className="size-2.5 shrink-0" />
          by {groupBy.join(', ')}
        </Badge>
      )}
    </div>
  )
}
