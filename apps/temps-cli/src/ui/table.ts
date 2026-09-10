// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import Table from 'cli-table3'
import chalk from 'chalk'
import { colors } from './output.js'
import { sanitizeTerminalText } from './terminal.js'

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
    head: columns.map((col) => colors.bold(sanitizeTerminalText(col.header))),
    colAligns: columns.map((col) => col.align ?? 'left'),
    colWidths: columns.map((col) => col.width ?? null),
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

      let strValue =
        value === null || value === undefined ? '' : sanitizeTerminalText(value)

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
    colWidths: [20, null],
  })

  for (const [key, value] of Object.entries(details)) {
    const displayValue =
      value === null || value === undefined
        ? colors.muted('not set')
        : sanitizeTerminalText(value)
    table.push([colors.muted(sanitizeTerminalText(key)), displayValue])
  }

  console.log(table.toString())
}

/**
 * Status badge formatter
 */
export function statusBadge(status: string): string {
  const safeStatus = sanitizeTerminalText(status)
  const statusColors: Record<string, (s: string) => string> = {
    running: chalk.green,
    active: chalk.green,
    success: chalk.green,
    healthy: chalk.green,
    completed: chalk.green,
    deployed: chalk.green,
    ready: chalk.green,
    pending: chalk.yellow,
    building: chalk.yellow,
    deploying: chalk.yellow,
    warning: chalk.yellow,
    built: chalk.yellow,
    stopped: chalk.gray,
    inactive: chalk.gray,
    paused: chalk.gray,
    failed: chalk.red,
    error: chalk.red,
    unhealthy: chalk.red,
    cancelled: chalk.red,
    degraded: chalk.yellow,
    failing: chalk.red,
    disabled: chalk.gray,
    never_delivered: chalk.gray,
  }

  const colorFn = statusColors[safeStatus.toLowerCase()] ?? chalk.white
  return colorFn(`● ${safeStatus}`)
}
