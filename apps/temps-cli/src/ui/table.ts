import Table from 'cli-table3'
import chalk from 'chalk'
import { colors } from './output.js'

export interface TableColumn<T> {
  header: string
  key?: keyof T
  accessor?: (item: T) => string | number | boolean | null | undefined
  align?: 'left' | 'center' | 'right'
  width?: number
  color?: (value: string, item: T) => string
}

export interface TableOptions {
  style?: 'default' | 'compact' | 'minimal' | 'borderless'
  maxWidth?: number
}

const stylePresets = {
  default: {
    chars: {
      top: '─',
      'top-mid': '┬',
      'top-left': '┌',
      'top-right': '┐',
      bottom: '─',
      'bottom-mid': '┴',
      'bottom-left': '└',
      'bottom-right': '┘',
      left: '│',
      'left-mid': '├',
      mid: '─',
      'mid-mid': '┼',
      right: '│',
      'right-mid': '┤',
      middle: '│',
    },
    style: {
      head: ['cyan'],
      border: ['gray'],
    },
  },
  compact: {
    chars: {
      top: '',
      'top-mid': '',
      'top-left': '',
      'top-right': '',
      bottom: '',
      'bottom-mid': '',
      'bottom-left': '',
      'bottom-right': '',
      left: '',
      'left-mid': '',
      mid: '',
      'mid-mid': '',
      right: '',
      'right-mid': '',
      middle: ' ',
    },
    style: {
      head: ['cyan', 'bold'],
      border: [],
      'padding-left': 0,
      'padding-right': 2,
    },
  },
  minimal: {
    chars: {
      top: '─',
      'top-mid': '─',
      'top-left': '',
      'top-right': '',
      bottom: '─',
      'bottom-mid': '─',
      'bottom-left': '',
      'bottom-right': '',
      left: '',
      'left-mid': '',
      mid: '─',
      'mid-mid': '─',
      right: '',
      'right-mid': '',
      middle: ' │ ',
    },
    style: {
      head: ['cyan'],
      border: ['gray'],
    },
  },
  borderless: {
    chars: {
      top: '',
      'top-mid': '',
      'top-left': '',
      'top-right': '',
      bottom: '',
      'bottom-mid': '',
      'bottom-left': '',
      'bottom-right': '',
      left: '',
      'left-mid': '',
      mid: '',
      'mid-mid': '',
      right: '',
      'right-mid': '',
      middle: '  ',
    },
    style: {
      head: ['cyan', 'bold'],
      border: [],
    },
  },
}

export function createTable<T>(
  data: T[],
  columns: TableColumn<T>[],
  options: TableOptions = {}
): string {
  const preset = stylePresets[options.style ?? 'default']

  const table = new Table({
    head: columns.map((col) => colors.bold(col.header)),
    colAligns: columns.map((col) => col.align ?? 'left'),
    colWidths: columns.map((col) => col.width),
    ...preset,
  })

  for (const item of data) {
    const row = columns.map((col) => {
      let value: string | number | boolean | null | undefined

      if (col.accessor) {
        value = col.accessor(item)
      } else if (col.key) {
        value = item[col.key] as string | number | boolean | null | undefined
      } else {
        value = ''
      }

      let strValue = value === null || value === undefined ? '' : String(value)

      if (col.color) {
        strValue = col.color(strValue, item)
      }

      return strValue
    })
    table.push(row)
  }

  return table.toString()
}

export function printTable<T>(
  data: T[],
  columns: TableColumn<T>[],
  options: TableOptions = {}
): void {
  if (data.length === 0) {
    console.log(colors.muted('  No data to display'))
    return
  }
  console.log(createTable(data, columns, options))
}

/**
 * Simple key-value table for displaying details
 */
export function detailsTable(
  details: Record<string, string | number | boolean | null | undefined>
): void {
  const table = new Table({
    ...stylePresets.borderless,
    colWidths: [20, undefined],
  })

  for (const [key, value] of Object.entries(details)) {
    const displayValue = value === null || value === undefined ? colors.muted('not set') : String(value)
    table.push([colors.muted(key), displayValue])
  }

  console.log(table.toString())
}

/**
 * Status badge formatter
 */
export function statusBadge(status: string): string {
  const statusColors: Record<string, (s: string) => string> = {
    running: chalk.green,
    active: chalk.green,
    success: chalk.green,
    healthy: chalk.green,
    pending: chalk.yellow,
    building: chalk.yellow,
    deploying: chalk.yellow,
    warning: chalk.yellow,
    stopped: chalk.gray,
    inactive: chalk.gray,
    failed: chalk.red,
    error: chalk.red,
    unhealthy: chalk.red,
  }

  const colorFn = statusColors[status.toLowerCase()] ?? chalk.white
  return colorFn(`● ${status}`)
}
