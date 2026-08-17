export interface ChartDateRange {
  from: Date
  to: Date
}

function chartValueToDate(value: unknown): Date | null {
  if (value instanceof Date) {
    return Number.isNaN(value.getTime()) ? null : value
  }
  if (typeof value !== 'number' && typeof value !== 'string') return null

  const date = new Date(value)
  return Number.isNaN(date.getTime()) ? null : date
}

export function orderedChartDateRange(
  first: unknown,
  second: unknown
): ChartDateRange | null {
  const firstDate = chartValueToDate(first)
  const secondDate = chartValueToDate(second)
  if (!firstDate || !secondDate) return null

  const firstTime = firstDate.getTime()
  const secondTime = secondDate.getTime()
  if (firstTime === secondTime) return null

  return firstTime < secondTime
    ? { from: firstDate, to: secondDate }
    : { from: secondDate, to: firstDate }
}
