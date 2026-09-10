// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { getIngestErrors } from '../../api/sdk.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, type TableColumn } from '../../ui/table.js'
import {
  newline,
  header,
  json as jsonOut,
  colors,
  formatRelativeTime,
  truncate,
} from '../../ui/output.js'
import type { IngestErrorSummary } from '../../api/types.gen.js'

interface IngestErrorsOptions {
  limit?: string
  json?: boolean
}

/**
 * `clickhouse_network` -> `ClickHouse network`, matching the console's wording.
 *
 * Product names are cased explicitly; a naive capitalize gives "Clickhouse",
 * which reads as a typo.
 */
const ERROR_CLASS_WORDS: Record<string, string> = {
  clickhouse: 'ClickHouse',
  postgres: 'Postgres',
  conn: 'connection',
  s3: 'S3',
  io: 'I/O',
}

function humanizeErrorClass(errorClass: string): string {
  const words = errorClass
    .split('_')
    .map((w) => ERROR_CLASS_WORDS[w] ?? w)
    .join(' ')
  return words.charAt(0).toUpperCase() + words.slice(1)
}

export async function otelIngestErrors(
  options: IngestErrorsOptions
): Promise<void> {
  await requireAuth()
  await setupClient()

  // Left undefined when not supplied so the server applies its own default
  // (20) and cap (100) — the CLI must not hard-code a second copy of those.
  const limit = options.limit ? parseInt(options.limit, 10) : undefined
  if (limit !== undefined && (Number.isNaN(limit) || limit < 1)) {
    throw new Error(`Invalid --limit "${options.limit}". Must be a positive integer.`)
  }

  const data = await withSpinner('Fetching ingest errors...', async () => {
    const { data, error } = await getIngestErrors({
      client,
      query: limit === undefined ? {} : { limit },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data
  })

  const errors = data?.errors ?? []

  if (options.json) {
    jsonOut({ count: errors.length, errors })
    return
  }

  header('OTel ingest errors')

  if (errors.length === 0) {
    newline()
    console.log(
      colors.muted('  No ingest errors recorded in the last 7 days.')
    )
    console.log(
      colors.muted(
        '  Entries appear here only when a storage write fails after all retries.'
      )
    )
    newline()
    return
  }

  const columns: TableColumn<IngestErrorSummary>[] = [
    { header: 'Signal', accessor: (e) => e.signal_type, align: 'left' },
    {
      header: 'Reason',
      accessor: (e) => humanizeErrorClass(e.error_class),
      align: 'left',
    },
    { header: 'Count', accessor: (e) => e.count, align: 'right' },
    {
      header: 'Last seen',
      accessor: (e) => formatRelativeTime(e.last_seen),
      align: 'right',
    },
    {
      header: 'Sample message',
      accessor: (e) => truncate(e.sample_message, 60),
      align: 'left',
    },
  ]

  printTable(errors, columns, { style: 'minimal' })
  newline()
  console.log(
    colors.muted(
      `  ${errors.length} failure group(s). Each entry means data was dropped after retries were exhausted.`
    )
  )
  newline()
}
