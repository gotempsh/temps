// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// `temps edge-rules`: the project's ordered request rules (ADR-071), checked
// by the proxy before a request reaches the app. Every change is live on
// every proxy within seconds of the command returning.

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  createEdgeRule,
  deleteEdgeRule,
  evaluateEdgeRules,
  getEdgeRule,
  getEnvironments,
  listEdgeRules,
  reorderEdgeRules,
  updateEdgeRule,
} from '../../api/sdk.gen.js'
import type { EdgeRuleListResponse, EdgeRuleResponse } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { promptConfirm } from '../../ui/prompts.js'
import { createTable } from '../../ui/table.js'
import { colors, header, icons, info, json, keyValue, newline, success, warning } from '../../ui/output.js'
import {
  describeDecision,
  modeOf,
  parseHeaders,
  parseJsonFlag,
  parseRulesFile,
  planApply,
  planIsEmpty,
  toSpec,
  type RuleSpec,
  type SavedRule,
} from './plan.js'

export function parseId(raw: string | undefined, label: string): number {
  const value = Number(raw)
  if (!raw || !Number.isInteger(value) || value <= 0) {
    throw new Error(`${label} must be a positive integer, got "${raw ?? ''}"`)
  }
  return value
}

export function parseMode(raw: string): 'enforce' | 'log_only' {
  const mode = raw.replace('-', '_')
  if (mode !== 'enforce' && mode !== 'log_only') {
    throw new Error(`--mode must be "enforce" or "log-only", got "${raw}"`)
  }
  return mode
}

function collect(value: string, previous: string[]): string[] {
  return [...previous, value]
}

async function connect(): Promise<void> {
  await requireAuth()
  await setupClient()
}

async function readFile(path: string): Promise<string> {
  const file = Bun.file(path)
  if (!(await file.exists())) throw new Error(`File not found: ${path}`)
  return file.text()
}

async function fetchList(projectId: number): Promise<EdgeRuleListResponse> {
  const { data, error } = await listEdgeRules({ client, path: { project_id: projectId } })
  if (error || !data) throw new Error(getErrorMessage(error))
  return data
}

function saved(rule: EdgeRuleResponse): SavedRule {
  return {
    id: rule.id,
    revision: rule.revision,
    name: rule.name,
    enabled: rule.enabled,
    mode: rule.mode,
    environment_id: rule.environment_id ?? null,
    expression: rule.expression,
    action: rule.action,
  }
}

function modeLabel(rule: EdgeRuleResponse): string {
  return rule.mode === 'enforce' ? colors.success('enforce') : colors.warning('log only')
}

function printRule(rule: EdgeRuleResponse): void {
  newline()
  header(`${icons.info} Edge rule ${rule.id}: ${rule.name}`)
  keyValue('Position', rule.position + 1)
  keyValue('Enabled', rule.enabled ? 'yes' : 'no')
  keyValue('Mode', rule.mode === 'enforce' ? 'enforce' : 'log only (records matches, never acts)')
  keyValue('Applies to', rule.environment_id == null ? 'all environments' : `environment ${rule.environment_id}`)
  keyValue('Rule', rule.summary)
  keyValue('Matches', `${rule.hits}${rule.last_hit_at ? ` (last ${rule.last_hit_at})` : ''}`)
  keyValue('Revision', rule.revision)
  newline()
}

/** Body for create: a whole rule from --file or from flags. */
export async function ruleFromOptions(options: RuleOptions): Promise<RuleSpec | Omit<RuleSpec, 'name'>> {
  if (options.file) {
    const raw = await readFile(options.file)
    const parsed = parseJsonFlag(raw, options.file) as Record<string, unknown>
    return {
      ...(parsed as unknown as RuleSpec),
      ...(options.name ? { name: options.name } : {}),
    }
  }
  if (!options.expression || !options.action) {
    throw new Error('Pass --file rule.json, or both --expression and --action as JSON')
  }
  return {
    ...(options.name ? { name: options.name } : {}),
    expression: parseJsonFlag(options.expression, '--expression'),
    action: parseJsonFlag(options.action, '--action'),
  }
}

interface ProjectOptions {
  projectId?: string
  json?: boolean
}

interface RuleOptions extends ProjectOptions {
  file?: string
  name?: string
  expression?: string
  action?: string
  mode?: string
  environmentId?: string
  allEnvironments?: boolean
  disabled?: boolean
  position?: string
  revision?: string
}

interface TestOptions extends ProjectOptions {
  url?: string
  method?: string
  header?: string[]
  ip?: string
  country?: string
  continent?: string
  asn?: string
  userAgent?: string
  botClass?: string
  environmentId?: string
  cleared?: boolean
  file?: string
}

interface ApplyOptions extends ProjectOptions {
  file?: string
  yes?: boolean
  dryRun?: boolean
}

export function registerEdgeRulesCommands(program: Command): void {
  const edgeRules = program
    .command('edge-rules')
    .alias('edge-rule')
    .description(
      'Block, allow, challenge, redirect or log requests before they reach the app. Rules run top to bottom; the first match decides',
    )

  const withProject = (command: Command): Command =>
    command.requiredOption('--project-id <id>', 'Project ID').option('--json', 'Output in JSON format')

  withProject(edgeRules.command('list').alias('ls').description('List the rules in evaluation order')).action(
    listAction,
  )

  withProject(edgeRules.command('get <rule-id>').description('Show one rule')).action(getAction)

  const ruleFlags = (command: Command): Command =>
    command
      .option('--file <path>', 'JSON file with the rule: {name?, expression, action, mode?, enabled?, environment_id?}')
      .option('--name <name>', 'Rule name (generated from the rule when omitted)')
      .option('--expression <json>', 'Match expression as JSON, e.g. {"combinator":"all","children":[...]}')
      .option('--action <json>', 'Action as JSON, e.g. {"type":"block","status":403}')
      .option('--environment-id <id>', 'Apply only to this environment')

  withProject(
    ruleFlags(
      edgeRules
        .command('create')
        .description('Add a rule. New rules start in log-only mode unless --mode enforce is given'),
    ),
  )
    .option('--mode <mode>', 'enforce or log-only (default: the file\'s mode, else log-only)')
    .option('--disabled', 'Create it disabled')
    .option('--position <n>', 'Insert at this 1-based position (default: last)')
    .action(createAction)

  withProject(ruleFlags(edgeRules.command('update <rule-id>').description('Change a rule; only the given parts change')))
    .option('--mode <mode>', 'enforce or log-only')
    .option('--all-environments', 'Apply to every environment')
    .option('--revision <n>', 'Fail if the rule changed since this revision (default: the current one)')
    .action(updateAction)

  withProject(edgeRules.command('delete <rule-id>').alias('rm').description('Delete a rule'))
    .option('-y, --yes', 'Skip confirmation')
    .action(deleteAction)

  withProject(edgeRules.command('move <rule-id>').description('Move a rule to another position'))
    .requiredOption('--to <n>', 'New 1-based position')
    .action(moveAction)

  const toggles: Array<[string, string, Record<string, unknown>]> = [
    ['enable', 'Enable a rule', { enabled: true }],
    ['disable', 'Disable a rule (kept, never evaluated)', { enabled: false }],
    ['enforce', 'Act on matches', { mode: 'enforce' }],
    ['log-only', 'Only record matches, never act on them', { mode: 'log_only' }],
  ]
  for (const [name, description, change] of toggles) {
    withProject(edgeRules.command(`${name} <rule-id>`).description(description)).action(
      (ruleId: string, options: ProjectOptions) => patchAction(ruleId, options, change),
    )
  }

  withProject(
    edgeRules.command('test').description('Dry-run a request against the saved rules (or a rules file) and show the trace'),
  )
    .requiredOption('--url <url>', 'Request URL or path, e.g. https://app.example.com/admin?x=1')
    .option('--method <method>', 'HTTP method', 'GET')
    .option('--header <header>', '"Name: value"; repeatable', collect, [] as string[])
    .option('--ip <ip>', 'Client address')
    .option('--country <code>', 'ISO country code, e.g. DE')
    .option('--continent <code>', 'Continent code, e.g. EU')
    .option('--asn <n>', 'Autonomous system number')
    .option('--user-agent <ua>', 'User agent')
    .option('--bot-class <class>', 'human, search_engine, ai_crawler, ai_agent or unknown_bot')
    .option('--environment-id <id>', 'Environment the request is for')
    .option('--cleared', 'The visitor already solved a challenge')
    .option('--file <path>', 'Evaluate these rules instead of the saved ones (same format as export)')
    .action(testAction)

  withProject(edgeRules.command('export').description('Print the rules as a file apply accepts')).option(
    '--output <path>',
    'Write to a file instead of stdout',
  ).action(exportAction)

  withProject(
    edgeRules
      .command('apply')
      .description(
        'Make the project rules match a file: rules are matched by name, changed ones updated, missing ones created, others deleted, then ordered as in the file',
      ),
  )
    .requiredOption('--file <path>', 'Rules file ({"rules": [...]}, as written by export)')
    .option('--dry-run', 'Show the plan without changing anything')
    .option('-y, --yes', 'Skip confirmation')
    .action(applyAction)
}

async function listAction(options: ProjectOptions): Promise<void> {
  await connect()
  const projectId = parseId(options.projectId, '--project-id')
  const list = await withSpinner(`Fetching edge rules for project ${projectId}...`, () => fetchList(projectId))
  if (options.json) {
    json(list)
    return
  }
  newline()
  header(`${icons.info} Edge rules for project ${projectId} (${list.rules.length}/${list.max_rules})`)
  if (list.rules.length === 0) {
    info('No rules: every request goes to the app. Add one with `temps edge-rules create`.')
    newline()
    return
  }
  console.log(
    createTable(list.rules, [
      { header: '#', accessor: (r) => r.position + 1, align: 'right' },
      { header: 'ID', key: 'id', align: 'right' },
      { header: 'Name', key: 'name' },
      { header: 'Rule', key: 'summary' },
      { header: 'Mode', accessor: (r) => modeLabel(r) },
      { header: 'Enabled', accessor: (r) => (r.enabled ? 'yes' : colors.muted('no')) },
      { header: 'Matches', key: 'hits', align: 'right' },
    ]),
  )
  newline()
}

async function getAction(ruleId: string, options: ProjectOptions): Promise<void> {
  await connect()
  const projectId = parseId(options.projectId, '--project-id')
  const id = parseId(ruleId, 'rule id')
  const { data, error } = await getEdgeRule({ client, path: { project_id: projectId, rule_id: id } })
  if (error || !data) throw new Error(getErrorMessage(error))
  if (options.json) json(data)
  else printRule(data)
}

async function createAction(options: RuleOptions): Promise<void> {
  await connect()
  const projectId = parseId(options.projectId, '--project-id')
  const spec = await ruleFromOptions(options)
  const fileMode = 'mode' in spec && spec.mode ? spec.mode : undefined
  const body = {
    ...spec,
    // An explicit --mode wins over the file; nothing given means log-only.
    mode: options.mode ? parseMode(options.mode) : (fileMode ?? 'log_only'),
    enabled: options.disabled ? false : ('enabled' in spec && spec.enabled !== undefined ? spec.enabled : true),
    environment_id:
      options.environmentId !== undefined
        ? parseId(options.environmentId, '--environment-id')
        : ('environment_id' in spec ? spec.environment_id : null) ?? null,
    position: options.position !== undefined ? parseId(options.position, '--position') - 1 : undefined,
  }
  const rule = await withSpinner('Creating edge rule...', async () => {
    const { data, error } = await createEdgeRule({ client, path: { project_id: projectId }, body: body as never })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(rule)
    return
  }
  success(`Created edge rule ${rule.id} "${rule.name}" at position ${rule.position + 1}`)
  info(rule.summary)
  if (rule.mode === 'log_only') {
    info(`It records matches without acting. Enforce it with: temps edge-rules enforce ${rule.id} --project-id ${projectId}`)
  }
}

async function update(
  projectId: number,
  ruleId: number,
  change: Record<string, unknown>,
  revision?: number,
): Promise<EdgeRuleResponse> {
  let current = revision
  if (current === undefined) {
    const { data, error } = await getEdgeRule({ client, path: { project_id: projectId, rule_id: ruleId } })
    if (error || !data) throw new Error(getErrorMessage(error))
    current = data.revision
  }
  const { data, error } = await updateEdgeRule({
    client,
    path: { project_id: projectId, rule_id: ruleId },
    body: { ...change, revision: current } as never,
  })
  if (error || !data) throw new Error(getErrorMessage(error))
  return data
}

async function updateAction(ruleId: string, options: RuleOptions): Promise<void> {
  await connect()
  const projectId = parseId(options.projectId, '--project-id')
  const id = parseId(ruleId, 'rule id')
  const change: Record<string, unknown> = {}
  if (options.file) {
    const spec = await ruleFromOptions(options)
    Object.assign(change, spec)
  } else {
    if (options.name) change.name = options.name
    if (options.expression) change.expression = parseJsonFlag(options.expression, '--expression')
    if (options.action) change.action = parseJsonFlag(options.action, '--action')
  }
  if (options.mode) change.mode = parseMode(options.mode)
  if (options.environmentId !== undefined) change.environment_id = parseId(options.environmentId, '--environment-id')
  if (options.allEnvironments) change.environment_id = null
  if (Object.keys(change).length === 0) {
    throw new Error('Nothing to change: pass --file, --name, --expression, --action, --mode or an environment flag')
  }
  const revision = options.revision !== undefined ? parseId(options.revision, '--revision') : undefined
  const rule = await withSpinner(`Updating edge rule ${id}...`, () => update(projectId, id, change, revision))
  if (options.json) json(rule)
  else {
    success(`Updated edge rule ${rule.id} "${rule.name}" (revision ${rule.revision})`)
    info(rule.summary)
  }
}

async function patchAction(ruleId: string, options: ProjectOptions, change: Record<string, unknown>): Promise<void> {
  await connect()
  const projectId = parseId(options.projectId, '--project-id')
  const id = parseId(ruleId, 'rule id')
  const rule = await withSpinner(`Updating edge rule ${id}...`, () => update(projectId, id, change))
  if (options.json) {
    json(rule)
    return
  }
  const state = !rule.enabled ? 'disabled' : rule.mode === 'enforce' ? 'enabled and enforced' : 'enabled, log only'
  success(`Edge rule ${rule.id} "${rule.name}" is now ${state}`)
}

async function deleteAction(ruleId: string, options: ProjectOptions & { yes?: boolean }): Promise<void> {
  await connect()
  const projectId = parseId(options.projectId, '--project-id')
  const id = parseId(ruleId, 'rule id')
  if (!options.yes) {
    const confirmed = await promptConfirm({ message: `Delete edge rule ${id} from project ${projectId}?`, default: false })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }
  await withSpinner(`Deleting edge rule ${id}...`, async () => {
    const { error } = await deleteEdgeRule({ client, path: { project_id: projectId, rule_id: id } })
    if (error) throw new Error(getErrorMessage(error))
  })
  success(`Deleted edge rule ${id}`)
}

/** The full order with `ruleId` moved to the 1-based `to`. */
export function moveInOrder(ids: number[], ruleId: number, to: number): number[] {
  const from = ids.indexOf(ruleId)
  if (from === -1) throw new Error(`Edge rule ${ruleId} is not in this project`)
  const next = ids.filter((id) => id !== ruleId)
  next.splice(Math.max(0, Math.min(to - 1, next.length)), 0, ruleId)
  return next
}

async function moveAction(ruleId: string, options: ProjectOptions & { to?: string }): Promise<void> {
  await connect()
  const projectId = parseId(options.projectId, '--project-id')
  const id = parseId(ruleId, 'rule id')
  const to = parseId(options.to, '--to')
  const list = await fetchList(projectId)
  const order = moveInOrder(
    list.rules.map((r) => r.id),
    id,
    to,
  )
  const result = await withSpinner(`Moving edge rule ${id}...`, async () => {
    const { data, error } = await reorderEdgeRules({
      client,
      path: { project_id: projectId },
      body: { rule_ids: order },
    })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) json(result)
  else success(`Edge rule ${id} is now at position ${order.indexOf(id) + 1} of ${order.length}`)
}

async function testAction(options: TestOptions): Promise<void> {
  await connect()
  const projectId = parseId(options.projectId, '--project-id')
  const asn = options.asn === undefined ? null : Number(options.asn.replace(/^AS/i, ''))
  if (asn !== null && (!Number.isInteger(asn) || asn < 0)) throw new Error(`--asn must be a number, got "${options.asn}"`)
  const rules = options.file ? parseRulesFile(await readFile(options.file), options.file).rules : undefined
  const body = {
    request: {
      method: (options.method ?? 'GET').toUpperCase(),
      url: options.url ?? '/',
      headers: parseHeaders(options.header ?? []),
      ip: options.ip ?? null,
      country: options.country ?? null,
      continent: options.continent ?? null,
      asn,
      user_agent: options.userAgent ?? null,
      bot_class: options.botClass ?? null,
      environment_id: options.environmentId ? parseId(options.environmentId, '--environment-id') : null,
      challenge_cleared: options.cleared === true,
    },
    rules,
  }
  const result = await withSpinner('Evaluating...', async () => {
    const { data, error } = await evaluateEdgeRules({ client, path: { project_id: projectId }, body: body as never })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(result)
    return
  }
  newline()
  header(`${icons.info} ${body.request.method} ${body.request.url}: ${describeDecision(result.decision as never)}`)
  info(result.summary)
  if (result.trace.length > 0) {
    console.log(
      createTable(result.trace, [
        { header: 'Rule', accessor: (t) => (t.rule_id > 0 ? t.rule_id : 'draft') },
        { header: 'Name', key: 'name' },
        { header: 'Outcome', accessor: (t) => String(t.outcome).replace(/_/g, ' ') },
      ]),
    )
  }
  newline()
}

async function exportAction(options: ProjectOptions & { output?: string }): Promise<void> {
  await connect()
  const projectId = parseId(options.projectId, '--project-id')
  const list = await fetchList(projectId)
  const file = { rules: list.rules.map((r) => toSpec(saved(r))) }
  const text = `${JSON.stringify(file, null, 2)}\n`
  if (options.output) {
    await Bun.write(options.output, text)
    success(`Wrote ${file.rules.length} rules to ${options.output}`)
  } else {
    process.stdout.write(text)
  }
}

/**
 * Refuses a rules file the server would refuse part-way through `apply`.
 * The writes are separate requests and deletes go first, so a rule rejected
 * on create would leave the project without the rules already deleted. The
 * dry run deserializes and validates drafts exactly as create does, and
 * checks the whole set against the pattern budget; it does not look up
 * environments, so those are checked here.
 */
async function preflight(projectId: number, desired: RuleSpec[], path: string): Promise<void> {
  const { error } = await evaluateEdgeRules({
    client,
    path: { project_id: projectId },
    body: {
      request: { method: 'GET', url: '/' },
      rules: desired.map((spec) => ({
        name: spec.name,
        enabled: spec.enabled ?? true,
        mode: modeOf(spec),
        environment_id: spec.environment_id ?? null,
        expression: spec.expression,
        action: spec.action,
      })),
    } as never,
  })
  if (error) throw new Error(`${path} was not applied; nothing changed. ${getErrorMessage(error)}`)

  const wanted = [...new Set(desired.map((spec) => spec.environment_id).filter((id): id is number => id != null))]
  if (wanted.length === 0) return
  const { data, error: envError } = await getEnvironments({ client, path: { project_id: projectId } })
  if (envError || !data) {
    throw new Error(`${path} was not applied; nothing changed. Listing environments: ${getErrorMessage(envError)}`)
  }
  const known = new Set(data.map((env) => env.id))
  const missing = wanted.filter((id) => !known.has(id))
  if (missing.length > 0) {
    throw new Error(
      `${path} was not applied; nothing changed. Project ${projectId} has no environment ${missing.join(', ')}`,
    )
  }
}

async function applyAction(options: ApplyOptions): Promise<void> {
  await connect()
  const projectId = parseId(options.projectId, '--project-id')
  const path = options.file ?? ''
  const desired = parseRulesFile(await readFile(path), path).rules
  const list = await fetchList(projectId)
  if (desired.length > list.max_rules) {
    throw new Error(`${path} has ${desired.length} rules; a project can have ${list.max_rules}`)
  }
  const plan = planApply(list.rules.map(saved), desired)
  const currentOrder = list.rules.map((r) => r.name)

  if (planIsEmpty(plan, currentOrder)) {
    if (options.json) json({ plan, applied: false })
    else success(`Project ${projectId} already matches ${path}`)
    return
  }
  // Before the plan is shown, so a dry run reports a file that cannot apply.
  await withSpinner('Checking the rules...', () => preflight(projectId, desired, path))

  if (!options.json) {
    newline()
    header(`${icons.info} Plan for project ${projectId}`)
    const mode = (spec: RuleSpec) =>
      modeOf(spec) === 'enforce' ? colors.error('[enforce]') : colors.muted('[log only]')
    plan.create.forEach((r) => info(`${colors.success('+ create')} ${r.name} ${mode(r)}`))
    plan.update.forEach((r) => info(`${colors.warning('~ update')} ${r.name} ${mode(r.spec)}`))
    plan.remove.forEach((r) => info(`${colors.error('- delete')} ${r.name} (${r.id})`))
    info(`${colors.muted('order:')} ${plan.order.join(' → ')}`)
    newline()
  }
  if (options.dryRun) {
    if (options.json) json({ plan, applied: false })
    return
  }
  if (!options.yes) {
    const confirmed = await promptConfirm({
      message: `Apply this plan to project ${projectId}? Changes are live within seconds.`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  const ids = new Map<string, number>(list.rules.map((r) => [r.name.toLowerCase(), r.id]))
  await withSpinner('Applying...', async () => {
    // Deletes first, so creates never trip the per-project limit.
    for (const rule of plan.remove) {
      const { error } = await deleteEdgeRule({ client, path: { project_id: projectId, rule_id: rule.id } })
      if (error) throw new Error(`Deleting "${rule.name}": ${getErrorMessage(error)}`)
    }
    for (const change of plan.update) {
      const { error } = await updateEdgeRule({
        client,
        path: { project_id: projectId, rule_id: change.id },
        body: {
          revision: change.revision,
          name: change.spec.name,
          enabled: change.spec.enabled ?? true,
          mode: modeOf(change.spec),
          environment_id: change.spec.environment_id ?? null,
          expression: change.spec.expression,
          action: change.spec.action,
        } as never,
      })
      if (error) throw new Error(`Updating "${change.name}": ${getErrorMessage(error)}`)
    }
    for (const spec of plan.create) {
      const { data, error } = await createEdgeRule({
        client,
        path: { project_id: projectId },
        body: {
          name: spec.name,
          enabled: spec.enabled ?? true,
          mode: modeOf(spec),
          environment_id: spec.environment_id ?? null,
          expression: spec.expression,
          action: spec.action,
        } as never,
      })
      if (error || !data) throw new Error(`Creating "${spec.name}": ${getErrorMessage(error)}`)
      ids.set(spec.name.toLowerCase(), data.id)
    }
    const order = plan.order.map((name) => ids.get(name.toLowerCase()))
    if (order.some((id) => id === undefined)) throw new Error('Could not resolve every rule id to order them')
    const { error } = await reorderEdgeRules({
      client,
      path: { project_id: projectId },
      body: { rule_ids: order as number[] },
    })
    if (error) throw new Error(`Ordering the rules: ${getErrorMessage(error)}`)
  })

  if (options.json) {
    json({ plan, applied: true })
    return
  }
  success(
    `Applied ${path}: ${plan.create.length} created, ${plan.update.length} updated, ${plan.remove.length} deleted, ${plan.unchanged.length} unchanged`,
  )
  const enforced = [...plan.create, ...plan.update.map((u) => u.spec)].filter((r) => modeOf(r) === 'enforce')
  if (enforced.length > 0) {
    warning(
      `${enforced.map((r) => r.name).join(', ')} ${enforced.length === 1 ? 'is' : 'are'} enforced now. Watch ${enforced.length === 1 ? 'it' : 'them'} with \`temps edge-rules list\`.`,
    )
  }
}
