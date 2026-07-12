import { getGroupedPageMetricsOptions } from '@/api/client/@tanstack/react-query.gen'
import type { GroupedPageMetric } from '@/api/client/types.gen'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { Tabs, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { cn } from '@/lib/utils'
import { useQuery } from '@tanstack/react-query'
import { iso31661Alpha2ToNumeric } from 'iso-3166/1-a2-to-1-n'
import { useMemo, useState } from 'react'
import { ComposableMap, Geographies, Geography } from 'react-simple-maps'
import worldTopo from '@/assets/geo/countries-110m.json'

type MapMetric = 'ttfb' | 'lcp' | 'fcp' | 'inp' | 'cls'

const MAP_METRICS: { value: MapMetric; label: string }[] = [
  { value: 'ttfb', label: 'TTFB' },
  { value: 'lcp', label: 'LCP' },
  { value: 'fcp', label: 'FCP' },
  { value: 'inp', label: 'INP' },
  { value: 'cls', label: 'CLS' },
]

// Same Core Web Vitals thresholds as the tiles/breakdown on this page.
const MAP_THRESHOLDS: Record<MapMetric, { good: number; poor: number }> = {
  ttfb: { good: 800, poor: 1800 },
  lcp: { good: 2500, poor: 4000 },
  fcp: { good: 1800, poor: 3000 },
  inp: { good: 200, poor: 500 },
  cls: { good: 0.1, poor: 0.25 },
}

type MapStatus = 'good' | 'needs-improvement' | 'poor'

function mapStatus(value: number, metric: MapMetric): MapStatus {
  const t = MAP_THRESHOLDS[metric]
  if (value <= t.good) return 'good'
  if (value >= t.poor) return 'poor'
  return 'needs-improvement'
}

const STATUS_FILL: Record<MapStatus, string> = {
  good: 'fill-emerald-500/60 hover:fill-emerald-500/85',
  'needs-improvement': 'fill-amber-500/60 hover:fill-amber-500/85',
  poor: 'fill-red-500/60 hover:fill-red-500/85',
}

const LEGEND: { status: MapStatus; label: string; swatch: string }[] = [
  { status: 'good', label: 'Good', swatch: 'bg-emerald-500/60' },
  {
    status: 'needs-improvement',
    label: 'Needs work',
    swatch: 'bg-amber-500/60',
  },
  { status: 'poor', label: 'Poor', swatch: 'bg-red-500/60' },
]

function formatMapValue(value: number, metric: MapMetric) {
  if (metric === 'cls') return value.toFixed(2)
  if (value >= 1000) return `${(value / 1000).toFixed(2)}s`
  return `${Math.round(value)}ms`
}

interface SpeedWorldMapProps {
  projectId: number
  environmentId: number | null
  startDate: string
  endDate: string
  device: 'desktop' | 'mobile'
  includeBots: boolean
  filters: Record<string, string | undefined>
  onCountryClick: (country: string) => void
}

interface HoverInfo {
  name: string
  x: number
  y: number
  row?: GroupedPageMetric
}

export function SpeedWorldMap({
  projectId,
  environmentId,
  startDate,
  endDate,
  device,
  includeBots,
  filters,
  onCountryClick,
}: SpeedWorldMapProps) {
  const [metric, setMetric] = useState<MapMetric>('ttfb')
  const [hover, setHover] = useState<HoverInfo | null>(null)

  const { data, isLoading } = useQuery({
    ...getGroupedPageMetricsOptions({
      query: {
        start_date: startDate,
        end_date: endDate,
        project_id: projectId,
        environment_id: environmentId!,
        device_type: device,
        include_bots: includeBots,
        group_by: 'country',
        ...filters,
      },
    }),
    enabled: environmentId !== null,
  })

  // Index country rows by the topojson's ISO numeric id via their alpha-2
  // country_code. Rows without a code ("Unknown") can't be drawn on the map.
  const byNumericId = useMemo(() => {
    const index: Record<string, GroupedPageMetric> = {}
    for (const row of data?.groups ?? []) {
      const numeric = row.country_code
        ? iso31661Alpha2ToNumeric[
            row.country_code as keyof typeof iso31661Alpha2ToNumeric
          ]
        : undefined
      if (numeric) index[numeric] = row
    }
    return index
  }, [data])

  const unlocatedEvents = useMemo(() => {
    return (data?.groups ?? [])
      .filter((g) => !g.country_code)
      .reduce((sum, g) => sum + g.events, 0)
  }, [data])

  return (
    <Card>
      <CardHeader className="pb-3">
        <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
          <div>
            <CardTitle className="text-base">World map</CardTitle>
            <CardDescription>
              Average {MAP_METRICS.find((m) => m.value === metric)?.label} by
              country · click a country to filter
            </CardDescription>
          </div>
          <Tabs value={metric} onValueChange={(v) => setMetric(v as MapMetric)}>
            <TabsList className="h-8">
              {MAP_METRICS.map((m) => (
                <TabsTrigger
                  key={m.value}
                  value={m.value}
                  className="h-6 px-2.5 text-xs"
                >
                  {m.label}
                </TabsTrigger>
              ))}
            </TabsList>
          </Tabs>
        </div>
      </CardHeader>
      <CardContent>
        {isLoading ? (
          <Skeleton className="h-[320px] w-full" />
        ) : (
          <div className="relative" onMouseLeave={() => setHover(null)}>
            <ComposableMap
              projection="geoEqualEarth"
              projectionConfig={{ scale: 155, center: [0, 8] }}
              width={880}
              height={400}
              style={{ width: '100%', height: 'auto' }}
            >
              <Geographies geography={worldTopo}>
                {({ geographies }) =>
                  geographies
                    // Antarctica has no visitors and dominates the canvas.
                    .filter((geo) => geo.id !== '010')
                    .map((geo) => {
                      const row = byNumericId[geo.id as string]
                      const value = row?.[metric]
                      const hasValue = value !== null && value !== undefined
                      return (
                        <Geography
                          key={geo.rsmKey}
                          geography={geo}
                          onMouseEnter={(e) =>
                            setHover({
                              name: row?.group_key ?? geo.properties.name,
                              x: e.clientX,
                              y: e.clientY,
                              row,
                            })
                          }
                          onMouseMove={(e) =>
                            setHover((prev) =>
                              prev
                                ? { ...prev, x: e.clientX, y: e.clientY }
                                : prev
                            )
                          }
                          onMouseLeave={() => setHover(null)}
                          onClick={() => {
                            if (row) onCountryClick(row.group_key)
                          }}
                          className={cn(
                            'stroke-border stroke-[0.5] outline-none transition-colors',
                            hasValue
                              ? cn(
                                  STATUS_FILL[mapStatus(value, metric)],
                                  'cursor-pointer'
                                )
                              : 'fill-muted'
                          )}
                        />
                      )
                    })
                }
              </Geographies>
            </ComposableMap>

            {/* Tooltip */}
            {hover && (
              <div
                className="pointer-events-none fixed z-50 rounded-md border bg-popover px-3 py-2 text-xs shadow-md"
                style={{ left: hover.x + 12, top: hover.y + 12 }}
              >
                <div className="font-medium">{hover.name}</div>
                {hover.row ? (
                  <div className="mt-0.5 text-muted-foreground">
                    {(() => {
                      const v = hover.row[metric]
                      return v !== null && v !== undefined
                        ? `${MAP_METRICS.find((m) => m.value === metric)?.label} ${formatMapValue(v, metric)} · ${hover.row.events.toLocaleString()} samples`
                        : `No ${metric.toUpperCase()} samples`
                    })()}
                  </div>
                ) : (
                  <div className="mt-0.5 text-muted-foreground">No data</div>
                )}
              </div>
            )}

            {/* Legend */}
            <div className="mt-2 flex flex-wrap items-center gap-4 text-xs text-muted-foreground">
              {LEGEND.map((l) => (
                <span
                  key={l.status}
                  className="inline-flex items-center gap-1.5"
                >
                  <span className={cn('h-2.5 w-2.5 rounded-sm', l.swatch)} />
                  {l.label}
                </span>
              ))}
              <span className="inline-flex items-center gap-1.5">
                <span className="h-2.5 w-2.5 rounded-sm bg-muted" />
                No data
              </span>
              {unlocatedEvents > 0 && (
                <span className="ml-auto">
                  {unlocatedEvents.toLocaleString()} samples without location
                </span>
              )}
            </div>
          </div>
        )}
      </CardContent>
    </Card>
  )
}
