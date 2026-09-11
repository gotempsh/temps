// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMemo, useState } from 'react'
import {
  LineChart,
  Line,
  XAxis,
  YAxis,
  CartesianGrid,
  ReferenceArea,
} from 'recharts'
import {
  ChartContainer,
  ChartTooltip,
  ChartTooltipContent,
} from '@/components/ui/chart'
import { SERIES_STROKE } from '@/components/charts/chart-colors'
import type { GlobalLogLine } from '@/api/client/types.gen'
import { LOG_LEVELS, logVolume } from '@/lib/log-explorer'
import { Button } from '@/components/ui/button'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'

const COLORS = {
  ERROR: SERIES_STROKE.poor,
  WARN: SERIES_STROKE.warn,
  INFO: SERIES_STROKE.neutral,
  DEBUG: 'var(--muted-foreground)',
  TRACE: SERIES_STROKE.good,
}
export function LogVolume({
  lines,
  onRange,
}: {
  lines: GlobalLogLine[]
  onRange: (from: string, to: string) => void
}) {
  const { buckets, step } = useMemo(() => logVolume(lines), [lines])
  const [table, setTable] = useState(false)
  const [start, setStart] = useState<number>()
  const [end, setEnd] = useState<number>()
  const select = (from: number, to: number) =>
    onRange(
      new Date(Math.min(from, to)).toISOString(),
      new Date(Math.max(from, to) + step).toISOString()
    )
  return (
    <section aria-label="Log volume" className="py-4">
      <div className="mb-2 flex flex-wrap items-center justify-between gap-2">
        <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
          <h2 className="text-sm font-semibold">Volume by level</h2>
          <span className="text-xs text-muted-foreground">
            Loaded page · {step / 60000} min buckets
          </span>
        </div>
        <Button
          variant="ghost"
          size="sm"
          className="h-7 text-xs"
          onClick={() => setTable(!table)}
        >
          {table ? 'Show volume chart' : 'Show volume table'}
        </Button>
      </div>
      {!buckets.length ? (
        <div className="flex h-36 items-center justify-center text-xs text-muted-foreground">
          Volume appears when log lines are loaded.
        </div>
      ) : table ? (
        <div className="[&>div]:max-h-44">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Time (UTC)</TableHead>
                {LOG_LEVELS.map((level) => (
                  <TableHead key={level}>{level}</TableHead>
                ))}
              </TableRow>
            </TableHeader>
            <TableBody>
              {buckets.map((bucket) => (
                <TableRow key={bucket.time}>
                  <TableCell>
                    <button
                      type="button"
                      className="text-xs underline"
                      aria-label={`Select time bucket ${new Date(bucket.time).toISOString()}`}
                      onClick={() => select(bucket.time, bucket.time)}
                    >
                      {new Date(bucket.time).toLocaleTimeString(undefined, {
                        timeZone: 'UTC',
                      })}
                    </button>
                  </TableCell>
                  {LOG_LEVELS.map((level) => (
                    <TableCell key={level}>{bucket[level]}</TableCell>
                  ))}
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      ) : (
        <div className="h-40 w-full min-w-0 select-none">
          <ChartContainer
            className="h-full w-full aspect-auto"
            config={Object.fromEntries(
              LOG_LEVELS.map((level) => [
                level,
                { label: level.toLowerCase(), color: COLORS[level] },
              ])
            )}
          >
            <LineChart
              data={buckets}
              margin={{ top: 10, right: 12, left: -12, bottom: 0 }}
              onMouseDown={(state) => {
                const time = Number(state.activeLabel)
                if (Number.isFinite(time)) {
                  setStart(time)
                  setEnd(time)
                }
              }}
              onMouseMove={(state) => {
                const time = Number(state.activeLabel)
                if (start !== undefined && Number.isFinite(time)) setEnd(time)
              }}
              onMouseUp={() => {
                if (start !== undefined && end !== undefined) select(start, end)
                setStart(undefined)
                setEnd(undefined)
              }}
              onMouseLeave={() => {
                setStart(undefined)
                setEnd(undefined)
              }}
            >
              <CartesianGrid
                vertical={false}
                strokeDasharray="3 3"
                strokeOpacity={0.6}
                stroke="var(--border)"
              />
              <XAxis
                dataKey="time"
                type="number"
                domain={['dataMin', 'dataMax']}
                tickFormatter={(time) =>
                  new Date(time).toLocaleTimeString(undefined, {
                    hour: '2-digit',
                    minute: '2-digit',
                    timeZone: 'UTC',
                  })
                }
                minTickGap={60}
                tick={{ fontSize: 12 }}
                axisLine={false}
                tickLine={false}
              />
              <YAxis
                allowDecimals={false}
                tick={{ fontSize: 12 }}
                axisLine={false}
                tickLine={false}
              />
              <ChartTooltip
                cursor={{ stroke: 'var(--border)', strokeDasharray: '3 3' }}
                content={
                  <ChartTooltipContent
                    indicator="line"
                    labelFormatter={(_value, payload) =>
                      `${new Date(Number(payload[0]?.payload.time)).toLocaleString(undefined, { timeZone: 'UTC' })} UTC`
                    }
                  />
                }
              />
              {LOG_LEVELS.map((level) => (
                <Line
                  key={level}
                  dataKey={level}
                  stroke={COLORS[level]}
                  type="monotone"
                  strokeWidth={2}
                  activeDot={{ r: 4, strokeWidth: 0 }}
                  dot={buckets.length === 1}
                  isAnimationActive={false}
                />
              ))}
              {start !== undefined && end !== undefined && (
                <ReferenceArea
                  x1={Math.min(start, end)}
                  x2={Math.max(start, end)}
                  fill="var(--primary)"
                  fillOpacity={0.1}
                />
              )}
            </LineChart>
          </ChartContainer>
        </div>
      )}
      <div className="mt-2 flex flex-wrap items-center gap-x-4 gap-y-1 text-[11px] text-muted-foreground">
        {LOG_LEVELS.map((level) => (
          <span key={level} className="inline-flex items-center gap-1.5">
            <span className="h-0.5 w-3" style={{ background: COLORS[level] }} />
            {level.toLowerCase()}{' '}
            <span className="font-mono text-foreground">
              {lines.filter((line) => line.level === level).length}
            </span>
          </span>
        ))}
        <span className="ms-auto">UTC · drag to narrow this search</span>
      </div>
    </section>
  )
}
