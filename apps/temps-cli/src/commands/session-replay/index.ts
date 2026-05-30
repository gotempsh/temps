import type { Command } from 'commander'
import chalk from 'chalk'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { requireProjectSlug } from '../../config/resolve-project.js'
import {
  getProjectBySlug,
  getProjectSessionReplays,
  getVisitorSessions,
  getSessionReplay,
  getSessionReplayEvents,
  deleteSessionReplay,
} from '../../api/sdk.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, type TableColumn } from '../../ui/table.js'
import {
  header,
  keyValue,
  newline,
  json as jsonOut,
  colors,
  info,
  success,
  formatRelativeTime,
} from '../../ui/output.js'
import type { SessionReplayWithVisitorDto, SessionEventDto } from '../../api/types.gen.js'

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function formatDuration(ms: number | null | undefined): string {
  if (ms == null) return chalk.gray('—')
  const s = Math.round(ms / 1000)
  if (s < 60) return `${s}s`
  const m = Math.floor(s / 60)
  const rem = s % 60
  return rem > 0 ? `${m}m ${rem}s` : `${m}m`
}

function deviceIcon(device: string | null | undefined): string {
  if (!device) return '?'
  const d = device.toLowerCase()
  if (d.includes('mobile')) return '📱'
  if (d.includes('tablet')) return '📟'
  return '🖥'
}

function paginationFooter(page: number, perPage: number, total: number): void {
  const totalPages = Math.ceil(total / perPage)
  if (totalPages > 1) {
    info(
      `Page ${chalk.bold(page)} of ${chalk.bold(totalPages)} · ${chalk.bold(total)} total sessions`
    )
    if (page < totalPages) {
      info(`Use ${chalk.cyan('--page ' + (page + 1))} to see the next page`)
    }
  } else {
    info(`${chalk.bold(total)} session${total !== 1 ? 's' : ''}`)
  }
}

// ---------------------------------------------------------------------------
// list sessions for a project
// ---------------------------------------------------------------------------

interface ListOptions {
  project?: string
  environmentId?: string
  page?: string
  perPage?: string
  json?: boolean
}

async function listSessions(options: ListOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const resolved = await requireProjectSlug(options.project)
  if (resolved.source !== 'flag') {
    info(`Using project ${colors.bold(resolved.slug)} (from ${resolved.source})`)
  }

  const { data: projectData, error: projectError } = await getProjectBySlug({
    client,
    path: { slug: resolved.slug },
  })
  if (projectError || !projectData) throw new Error(`Project "${resolved.slug}" not found`)

  const projectId = projectData.id
  const page = options.page ? parseInt(options.page, 10) : 1
  const perPage = options.perPage ? parseInt(options.perPage, 10) : 25
  const environmentId = options.environmentId ? parseInt(options.environmentId, 10) : undefined

  const result = await withSpinner('Fetching session replays…', async () => {
    const { data, error } = await getProjectSessionReplays({
      client,
      query: {
        project_id: projectId,
        page,
        per_page: perPage,
        ...(environmentId != null && { environment_id: environmentId }),
      },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data
  })

  const sessions = result?.sessions ?? []
  const total = result?.total_count ?? sessions.length

  if (options.json) {
    jsonOut(result)
    return
  }

  if (sessions.length === 0) {
    info('No session replays found.')
    return
  }

  header(`Session Replays · ${resolved.slug}`)
  newline()

  const columns: TableColumn<SessionReplayWithVisitorDto>[] = [
    {
      header: 'ID',
      accessor: (s) => s.session_replay_id.slice(0, 8),
      width: 10,
    },
    {
      header: 'Dev',
      accessor: (s) => deviceIcon(s.device_type),
      width: 5,
    },
    {
      header: 'Browser',
      accessor: (s) => [s.browser, s.browser_version].filter(Boolean).join(' ') || '—',
      width: 18,
    },
    {
      header: 'OS',
      accessor: (s) => s.operating_system ?? '—',
      width: 14,
    },
    {
      header: 'Duration',
      accessor: (s) => formatDuration(s.duration),
      align: 'right',
      width: 10,
    },
    {
      header: 'Country',
      accessor: (s) => s.visitor_country_code ?? '—',
      width: 8,
    },
    {
      header: 'URL',
      accessor: (s) => (s.url ? s.url.replace(/^https?:\/\/[^/]+/, '') || '/' : '—'),
      width: 28,
    },
    {
      header: 'Started',
      accessor: (s) => (s.created_at ? formatRelativeTime(s.created_at) : '—'),
      width: 14,
    },
    {
      header: 'Visitor',
      accessor: (s) => `#${s.visitor_id}`,
      width: 10,
    },
  ]

  printTable(sessions, columns, { style: 'minimal' })
  newline()
  paginationFooter(page, perPage, total)
}

// ---------------------------------------------------------------------------
// list sessions for a specific visitor
// ---------------------------------------------------------------------------

interface ListVisitorOptions {
  page?: string
  perPage?: string
  json?: boolean
}

async function listVisitorSessions(visitorId: string, options: ListVisitorOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const vid = parseInt(visitorId, 10)
  if (isNaN(vid)) throw new Error('--visitor-id must be a number')

  const page = options.page ? parseInt(options.page, 10) : 1
  const perPage = options.perPage ? parseInt(options.perPage, 10) : 25

  const result = await withSpinner(`Fetching sessions for visitor #${vid}…`, async () => {
    const { data, error } = await getVisitorSessions({
      client,
      path: { visitor_id: vid },
      query: { page, per_page: perPage },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data
  })

  const sessions = result?.sessions ?? []
  const total = result?.total_count ?? sessions.length

  if (options.json) {
    jsonOut(result)
    return
  }

  if (sessions.length === 0) {
    info(`No sessions found for visitor #${vid}.`)
    return
  }

  header(`Sessions · Visitor #${vid}`)
  newline()

  const columns: TableColumn<SessionReplayWithVisitorDto>[] = [
    { header: 'ID', accessor: (s) => s.session_replay_id.slice(0, 8), width: 10 },
    { header: 'Dev', accessor: (s) => deviceIcon(s.device_type), width: 5 },
    { header: 'Browser', accessor: (s) => s.browser ?? '—', width: 16 },
    { header: 'Duration', accessor: (s) => formatDuration(s.duration), align: 'right', width: 10 },
    { header: 'URL', accessor: (s) => (s.url ? s.url.replace(/^https?:\/\/[^/]+/, '') || '/' : '—'), width: 32 },
    { header: 'Started', accessor: (s) => (s.created_at ? formatRelativeTime(s.created_at) : '—'), width: 14 },
  ]

  printTable(sessions, columns, { style: 'minimal' })
  newline()
  paginationFooter(page, perPage, total)
}

// ---------------------------------------------------------------------------
// show session metadata
// ---------------------------------------------------------------------------

interface ShowOptions {
  json?: boolean
}

async function showSession(visitorId: string, sessionId: string, options: ShowOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const vid = parseInt(visitorId, 10)
  const sid = parseInt(sessionId, 10)
  if (isNaN(vid)) throw new Error('visitor-id must be a number')
  if (isNaN(sid)) throw new Error('session-id must be a number')

  const result = await withSpinner('Fetching session…', async () => {
    const { data, error } = await getSessionReplay({
      client,
      path: { visitor_id: vid, session_id: sid },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data
  })

  const s = result?.session
  if (!s) throw new Error('Session not found')

  if (options.json) {
    jsonOut(result)
    return
  }

  header(`Session ${s.session_replay_id}`)
  newline()
  keyValue('Session ID', s.session_replay_id)
  keyValue('Visitor ID', String(s.visitor_id))
  keyValue('Duration', formatDuration(s.duration))
  keyValue('URL', s.url ?? '—')
  keyValue('Browser', [s.browser, s.browser_version].filter(Boolean).join(' ') || '—')
  keyValue('OS', [s.operating_system, s.operating_system_version].filter(Boolean).join(' ') || '—')
  keyValue('Device', s.device_type ?? '—')
  keyValue('Viewport', s.viewport_width && s.viewport_height ? `${s.viewport_width}×${s.viewport_height}` : '—')
  keyValue('Screen', s.screen_width && s.screen_height ? `${s.screen_width}×${s.screen_height}` : '—')
  keyValue('Language', s.language ?? '—')
  keyValue('Timezone', s.timezone ?? '—')
  keyValue('Country', [s.visitor_country, s.visitor_country_code ? `(${s.visitor_country_code})` : null].filter(Boolean).join(' ') || '—')
  keyValue('City', s.visitor_city ?? '—')
  keyValue('Region', s.visitor_region ?? '—')
  keyValue('Started', s.created_at ? formatRelativeTime(s.created_at) : '—')
  keyValue('First seen', formatRelativeTime(s.visitor_first_seen))
  keyValue('Last seen', formatRelativeTime(s.visitor_last_seen))
}

// ---------------------------------------------------------------------------
// download events for a session (paginated display, full JSON download)
// ---------------------------------------------------------------------------

interface EventsOptions {
  output?: string
  json?: boolean
  limit?: string
  page?: string
}

function eventTypeName(type: number | null | undefined): string {
  // rrweb event types: https://github.com/rrweb-io/rrweb/blob/master/packages/types/src/index.ts
  const names: Record<number, string> = {
    0: 'DomContentLoaded',
    1: 'Load',
    2: 'FullSnapshot',
    3: 'IncrementalSnapshot',
    4: 'Meta',
    5: 'Custom',
    6: 'Plugin',
  }
  if (type == null) return '—'
  return names[type] ?? `Type${type}`
}

async function downloadEvents(visitorId: string, sessionId: string, options: EventsOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const vid = parseInt(visitorId, 10)
  const sid = parseInt(sessionId, 10)
  if (isNaN(vid)) throw new Error('visitor-id must be a number')
  if (isNaN(sid)) throw new Error('session-id must be a number')

  const result = await withSpinner('Fetching session events…', async () => {
    const { data, error } = await getSessionReplayEvents({
      client,
      path: { visitor_id: vid, session_id: sid },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data
  })

  const allEvents = result?.events ?? []
  const session = result?.session

  if (options.json) {
    jsonOut(result)
    return
  }

  // If --output is given, write to file
  if (options.output) {
    const { writeFileSync } = await import('node:fs')
    const payload = JSON.stringify(result, null, 2)
    writeFileSync(options.output, payload, 'utf8')
    success(`Wrote ${allEvents.length} events to ${chalk.cyan(options.output)}`)
    return
  }

  // Paginated terminal display
  const pageSize = options.limit ? parseInt(options.limit, 10) : 50
  const page = options.page ? parseInt(options.page, 10) : 1
  const start = (page - 1) * pageSize
  const events = allEvents.slice(start, start + pageSize)
  const totalPages = Math.ceil(allEvents.length / pageSize)

  header(`Events · Session ${session?.session_replay_id?.slice(0, 8) ?? sid}`)
  if (session) {
    newline()
    keyValue('URL', session.url ?? '—')
    keyValue('Duration', formatDuration(session.duration))
    keyValue('Total events', String(allEvents.length))
  }
  newline()

  if (allEvents.length === 0) {
    info('No events recorded for this session.')
    return
  }

  let rowIndex = start
  const columns: TableColumn<SessionEventDto>[] = [
    { header: '#', accessor: () => String(++rowIndex), width: 5, align: 'right' },
    { header: 'ID', key: 'id', width: 8 },
    { header: 'Type', accessor: (e) => eventTypeName(e.event_type), width: 20 },
    {
      header: 'Timestamp',
      accessor: (e) => {
        // timestamp is ms since epoch
        const d = new Date(e.timestamp)
        return isNaN(d.getTime()) ? String(e.timestamp) : d.toISOString().replace('T', ' ').slice(0, 23)
      },
      width: 24,
    },
    {
      header: 'Data (preview)',
      accessor: (e) => {
        const s = JSON.stringify(e.data)
        return s.length > 60 ? s.slice(0, 57) + '…' : s
      },
      width: 60,
    },
  ]

  printTable(events, columns, { style: 'minimal' })
  newline()

  if (totalPages > 1) {
    info(`Page ${chalk.bold(page)} of ${chalk.bold(totalPages)} · ${chalk.bold(allEvents.length)} total events`)
    if (page < totalPages) {
      info(`Use ${chalk.cyan('--page ' + (page + 1))} to see the next page`)
    }
    newline()
    info(`To download all events: ${chalk.cyan(`bunx @temps-sdk/cli session-replay events ${visitorId} ${sessionId} --output events.json`)}`)
  }
}

// ---------------------------------------------------------------------------
// delete a session
// ---------------------------------------------------------------------------

interface DeleteOptions {
  yes?: boolean
}

async function deleteSession(visitorId: string, sessionId: string, options: DeleteOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const vid = parseInt(visitorId, 10)
  if (isNaN(vid)) throw new Error('visitor-id must be a number')

  if (!options.yes) {
    const { promptConfirm } = await import('../../ui/prompts.js')
    const ok = await promptConfirm({
      message: `Delete session ${chalk.cyan(sessionId)} for visitor #${vid}? This cannot be undone.`,
    })
    if (!ok) {
      info('Cancelled.')
      return
    }
  }

  await withSpinner('Deleting session…', async () => {
    const { error } = await deleteSessionReplay({
      client,
      path: { visitor_id: vid, session_id: sessionId },
    })
    if (error) throw new Error(getErrorMessage(error))
  })

  success(`Session ${chalk.cyan(sessionId)} deleted.`)
}

// ---------------------------------------------------------------------------
// Command registration
// ---------------------------------------------------------------------------

export function registerSessionReplayCommands(program: Command): void {
  const sr = program
    .command('session-replay')
    .aliases(['sessions', 'replay'])
    .description('Manage session replay recordings')

  // ── list ─────────────────────────────────────────────────────────────────
  sr.command('list')
    .aliases(['ls'])
    .description('List session replays for a project')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('--environment-id <id>', 'Filter by environment ID')
    .option('--page <n>', 'Page number (default: 1)', '1')
    .option('--per-page <n>', 'Sessions per page (default: 25, max: 100)', '25')
    .option('--json', 'Output raw JSON')
    .action(listSessions)

  // ── visitor ───────────────────────────────────────────────────────────────
  sr.command('visitor <visitor-id>')
    .description('List session replays for a specific visitor')
    .option('--page <n>', 'Page number (default: 1)', '1')
    .option('--per-page <n>', 'Sessions per page (default: 25)', '25')
    .option('--json', 'Output raw JSON')
    .action(listVisitorSessions)

  // ── show ──────────────────────────────────────────────────────────────────
  sr.command('show <visitor-id> <session-id>')
    .description('Show session metadata (use numeric session ID from list)')
    .option('--json', 'Output raw JSON')
    .action(showSession)

  // ── events ────────────────────────────────────────────────────────────────
  sr.command('events <visitor-id> <session-id>')
    .description('Download or page through all rrweb events for a session')
    .option('--page <n>', 'Page of events to display (default: 1)', '1')
    .option('--limit <n>', 'Events per page (default: 50)', '50')
    .option('--output <file>', 'Write all events as JSON to a file (skips paged display)')
    .option('--json', 'Print all events as JSON to stdout')
    .action(downloadEvents)

  // ── delete ────────────────────────────────────────────────────────────────
  sr.command('delete <visitor-id> <session-id>')
    .aliases(['rm'])
    .description('Delete a session replay')
    .option('-y, --yes', 'Skip confirmation prompt')
    .action(deleteSession)
}
