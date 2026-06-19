import { runLoad, formatLoadResult, type LoadResult } from '../lib/load.ts'

export interface LoadCommandOptions {
  requests?: string
  concurrency?: string
  duration?: string
  method?: string
  header?: string[]
  timeout?: string
  json?: boolean
}

/** Parse `-H "Key: Value"` repeatable flags into a header record. */
function parseHeaders(raw: string[] | undefined): Record<string, string> {
  const headers: Record<string, string> = {}
  for (const h of raw ?? []) {
    const idx = h.indexOf(':')
    if (idx > 0) headers[h.slice(0, idx).trim()] = h.slice(idx + 1).trim()
  }
  return headers
}

/** Parse a duration like "60s", "2m", or a bare number (seconds). */
function parseDuration(raw: string | undefined): number | undefined {
  if (!raw) return undefined
  const m = /^(\d+)(ms|s|m)?$/.exec(raw.trim())
  if (!m) throw new Error(`Invalid duration: ${raw} (use e.g. 60s, 2m, 500ms)`)
  const n = Number(m[1])
  switch (m[2]) {
    case 'ms':
      return n
    case 'm':
      return n * 60_000
    default:
      return n * 1000
  }
}

export async function loadCommand(url: string, opts: LoadCommandOptions): Promise<void> {
  const concurrency = Number(opts.concurrency ?? '50')
  const durationMs = parseDuration(opts.duration)
  const requests = durationMs ? undefined : Number(opts.requests ?? '1000')
  const timeoutMs = opts.timeout ? Number(opts.timeout) : undefined

  if (!opts.json) {
    const plan = durationMs ? `${durationMs / 1000}s` : `${requests} requests`
    process.stderr.write(`Load: ${plan} @ concurrency ${concurrency} -> ${url}\n`)
  }

  const result: LoadResult = await runLoad({
    url,
    requests,
    durationMs,
    concurrency,
    method: opts.method,
    headers: parseHeaders(opts.header),
    timeoutMs,
    onProgress: opts.json
      ? undefined
      : (completed, total) => {
          const pct = total ? ` (${Math.round((completed / total) * 100)}%)` : ''
          process.stderr.write(`\r  ${completed} done${pct}   `)
        },
  })

  if (!opts.json) process.stderr.write('\n')

  if (opts.json) {
    process.stdout.write(JSON.stringify(result, null, 2) + '\n')
  } else {
    process.stdout.write(formatLoadResult(result) + '\n')
  }

  // Non-zero exit when any request errored, so CI can gate on it.
  if (!result.ok) process.exitCode = 1
}
