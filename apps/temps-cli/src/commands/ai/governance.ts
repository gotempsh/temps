import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import type { ProblemDetails } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, type TableColumn } from '../../ui/table.js'
import { promptConfirm } from '../../ui/prompts.js'
import { newline, header, icons, json, colors, success, info, warning, keyValue } from '../../ui/output.js'

// ============================================================================
// Hand-written request/response shapes
// ============================================================================
//
// AI gateway governance is implemented by the temps-ai-gateway plugin crate,
// which is not part of this repository, so its schema does not exist in
// `openapi.json` and must never be added there (see "Regenerating the
// OpenAPI clients" in the root CLAUDE.md). These interfaces are hand
// maintained to mirror the plugin's serde structs exactly — keep them in
// sync by hand if that shape ever changes.

export interface GovernanceConfigResponse {
  id: number
  scope: string
  allowed_models: string[] | null
  max_requests_per_minute: number | null
  max_cost_per_month_microcents: number | null
  created_at: string
  updated_at: string
}

export interface SetGovernanceBody {
  allowed_models?: string[] | null
  max_requests_per_minute?: number | null
  max_cost_per_month_microcents?: number | null
}

// ============================================================================
// 403 / unavailable handling
// ============================================================================
//
// This routes through a plugin that may not be installed/enabled on a given
// server. Every one of these routes 404s when the plugin isn't present, and
// every one requires the AiGatewayWrite permission (even the read/list
// routes, since the response exposes budget and allowlist details) — both
// are deliberate cases to surface clearly rather than as a generic error.

const GOVERNANCE_UNAVAILABLE_MESSAGE =
  "AI gateway governance isn't available on this server — the endpoint returned 404. " +
  "This feature requires the AI gateway plugin, which isn't installed on this Temps instance."

const GOVERNANCE_FORBIDDEN_MESSAGE =
  'Permission denied — AI gateway governance requires the AiGatewayWrite permission on your ' +
  'credential (this applies to reads too, since the response exposes budget and allowlist details).'

function governanceErrorMessage(response: Response | undefined, error: unknown): string {
  if (response?.status === 404) {
    return GOVERNANCE_UNAVAILABLE_MESSAGE
  }
  if (response?.status === 403) {
    return GOVERNANCE_FORBIDDEN_MESSAGE
  }
  return getErrorMessage(error)
}

function throwGovernanceError(response: Response | undefined, error: unknown): never {
  throw new Error(governanceErrorMessage(response, error))
}

// ============================================================================
// Money / model-list helpers (unit tested)
// ============================================================================

// 1,000,000 microcents = $0.01, so $1.00 = 100,000,000 microcents.
const MICROCENTS_PER_DOLLAR = 100_000_000

/** Convert a human dollar amount (e.g. "50.00") to integer microcents for the API. */
export function dollarsToMicrocents(dollars: string): number {
  const parsed = Number(dollars)
  if (!Number.isFinite(parsed) || parsed < 0) {
    throw new Error(`Invalid --monthly-budget "${dollars}": expected a non-negative dollar amount, e.g. 50.00`)
  }
  return Math.round(parsed * MICROCENTS_PER_DOLLAR)
}

/** Convert microcents from the API back to a dollar amount for display. */
export function microcentsToDollars(microcents: number): string {
  return (microcents / MICROCENTS_PER_DOLLAR).toFixed(2)
}

/** Parse `--models` into the allowlist shape the API expects: `null` (all models allowed,
 * the default), `[]` (block all — triggered by the "none" sentinel), or an explicit list. */
export function parseAllowedModels(value: string | undefined): string[] | null | undefined {
  if (value === undefined) return undefined
  const trimmed = value.trim()
  if (trimmed.length === 0) {
    throw new Error('--models cannot be empty — pass a comma-separated list, "none" to block all models, or omit the flag entirely to allow all models')
  }
  if (trimmed.toLowerCase() === 'none') {
    return []
  }
  const models = trimmed.split(',').map((m) => m.trim()).filter((m) => m.length > 0)
  if (models.length === 0) {
    throw new Error('--models must contain at least one model id, or use "none" to block all models')
  }
  return models
}

export function formatAllowedModels(models: string[] | null): string {
  if (models === null) return colors.muted('all models')
  if (models.length === 0) return colors.error('none (blocked)')
  return models.join(', ')
}

interface SetOptions {
  models?: string
  rpm?: string
  monthlyBudget?: string
  json?: boolean
}

/** Build the PUT body from `set`'s options — only fields the user actually passed are
 * included, so unspecified fields are left untouched by the server's partial-update semantics. */
export function buildSetGovernanceBody(options: SetOptions): SetGovernanceBody {
  const body: SetGovernanceBody = {}

  const allowedModels = parseAllowedModels(options.models)
  if (allowedModels !== undefined) {
    body.allowed_models = allowedModels
  }

  if (options.rpm !== undefined) {
    const rpm = parseInt(options.rpm, 10)
    if (isNaN(rpm) || rpm < 0) {
      throw new Error(`Invalid --rpm "${options.rpm}": expected a non-negative integer`)
    }
    body.max_requests_per_minute = rpm
  }

  if (options.monthlyBudget !== undefined) {
    body.max_cost_per_month_microcents = dollarsToMicrocents(options.monthlyBudget)
  }

  return body
}

// ============================================================================
// Display helpers
// ============================================================================

function printGovernanceDetails(policy: GovernanceConfigResponse): void {
  newline()
  header(`${icons.info} Governance policy: ${policy.scope}`)
  keyValue('ID', policy.id)
  keyValue('Allowed models', formatAllowedModels(policy.allowed_models))
  keyValue(
    'Requests per minute',
    policy.max_requests_per_minute === null ? colors.muted('unlimited') : policy.max_requests_per_minute
  )
  keyValue(
    'Monthly budget',
    policy.max_cost_per_month_microcents === null
      ? colors.muted('unlimited')
      : `$${microcentsToDollars(policy.max_cost_per_month_microcents)}`
  )
  keyValue('Created', policy.created_at)
  keyValue('Updated', policy.updated_at)
  newline()
}

// ============================================================================
// Commander wiring
// ============================================================================

const SCOPE_HELP =
  'Scope: "instance", "project:<id>", "environment:<id>", or "token:<id>"'

export function registerAiGovernanceCommands(ai: Command): void {
  const governance = ai
    .command('governance')
    .description('Manage AI gateway governance policies (model allowlists, rate limits, spend caps) per scope')

  governance
    .command('list')
    .alias('ls')
    .description('List all AI gateway governance policies')
    .option('--json', 'Output in JSON format')
    .action(listGovernanceAction)

  governance
    .command('set <scope>')
    .description(`Create or update a governance policy for a scope. ${SCOPE_HELP}`)
    .option(
      '--models <comma-separated-list>',
      'Comma-separated model ids to allow. Use "none" to block all models. Omit this flag to leave the allowlist unset (default: all models allowed).'
    )
    .option('--rpm <number>', 'Max requests per minute')
    .option('--monthly-budget <dollars>', 'Max spend per calendar month, in dollars (e.g. 50.00)')
    .option('--json', 'Output in JSON format')
    .action(setGovernanceAction)

  governance
    .command('unset <scope>')
    .description(`Remove the governance policy for a scope, lifting its spend/rate limits. ${SCOPE_HELP}`)
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation prompts (alias for --force)')
    .action(unsetGovernanceAction)
}

// ============================================================================
// Actions
// ============================================================================

async function listGovernanceAction(options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const result = await withSpinner('Fetching AI gateway governance policies...', async () => {
    const { data, error, response } = await client.get<GovernanceConfigResponse[], ProblemDetails>({
      url: 'ai/governance',
    })
    if (error || !data) {
      throwGovernanceError(response, error)
    }
    return data
  })

  if (options.json) {
    json(result)
    return
  }

  newline()
  header(`${icons.info} AI Gateway Governance Policies (${result.length})`)

  if (result.length === 0) {
    info('No governance policies configured — every scope currently has unlimited access.')
    info('Run: temps ai governance set instance --rpm 100 --monthly-budget 50.00')
    newline()
    return
  }

  const columns: TableColumn<GovernanceConfigResponse>[] = [
    { header: 'Scope', key: 'scope', color: (v) => colors.bold(v) },
    { header: 'Allowed models', accessor: (p) => formatAllowedModels(p.allowed_models) },
    {
      header: 'RPM',
      accessor: (p) => (p.max_requests_per_minute === null ? 'unlimited' : p.max_requests_per_minute.toString()),
    },
    {
      header: 'Monthly budget',
      accessor: (p) =>
        p.max_cost_per_month_microcents === null ? 'unlimited' : `$${microcentsToDollars(p.max_cost_per_month_microcents)}`,
    },
  ]

  printTable(result, columns, { style: 'minimal' })
  newline()
}

async function setGovernanceAction(scope: string, options: SetOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  let body: SetGovernanceBody
  try {
    body = buildSetGovernanceBody(options)
  } catch (err) {
    warning(getErrorMessage(err))
    return
  }

  if (Object.keys(body).length === 0) {
    warning('No fields to set — pass at least one of --models, --rpm, or --monthly-budget')
    return
  }

  const policy = await withSpinner(`Setting governance policy for "${scope}"...`, async () => {
    const { data, error, response } = await client.put<GovernanceConfigResponse, ProblemDetails>({
      url: 'ai/governance/{scope}',
      path: { scope },
      body,
    })
    if (error || !data) {
      throwGovernanceError(response, error)
    }
    return data
  })

  if (options.json) {
    json(policy)
    return
  }

  success(`Governance policy for "${scope}" updated`)
  printGovernanceDetails(policy)
}

async function unsetGovernanceAction(scope: string, options: { force?: boolean; yes?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const skipConfirmation = options.force || options.yes

  if (!skipConfirmation) {
    const confirmed = await promptConfirm({
      message: `Remove the governance policy for "${scope}"? This lifts its model allowlist, rate limit, and spend cap.`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  await withSpinner(`Removing governance policy for "${scope}"...`, async () => {
    const { error, response } = await client.delete<void, ProblemDetails>({
      url: 'ai/governance/{scope}',
      path: { scope },
    })
    if (error) {
      throwGovernanceError(response, error)
    }
  })

  success(`Governance policy for "${scope}" removed`)
}
