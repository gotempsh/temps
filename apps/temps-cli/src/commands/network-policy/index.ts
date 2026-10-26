// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  listRules,
  getRule,
  createRule,
  deleteRule,
  getEffectivePolicy,
  setProfile,
} from '../../api/sdk.gen.js'
import type {
  CreateNetworkPolicyRuleRequest,
  NetworkPolicyRuleResponse,
  ProfileResponse,
  SetProfileRequest,
} from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, type TableColumn } from '../../ui/table.js'
import { promptConfirm } from '../../ui/prompts.js'
import {
  newline,
  header,
  icons,
  json,
  colors,
  success,
  info,
  warning,
  keyValue,
  formatRelativeTime,
} from '../../ui/output.js'

/** Workload kinds a rule or profile can be attached to. */
export const SUBJECT_KINDS = ['deployment', 'sandbox', 'service', 'function', 'job'] as const
export const DIRECTIONS = ['egress', 'ingress'] as const
export const ACTIONS = ['allow', 'deny'] as const
/** `host` is accepted by the parser but refused by the server (409) until a
 * node runs the shared egress proxy; the server's message explains why. */
export const PEER_KINDS = ['service', 'workload', 'cidr', 'host'] as const
export const PROTOCOLS = ['tcp', 'udp', 'any'] as const
export const PROFILES = ['open', 'restricted', 'none'] as const
export const ORIGINS = ['link', 'operator', 'system'] as const

/** Return `value` if it is one of `allowed`, otherwise throw a message naming
 * the flag and every accepted value. Case-sensitive, like the server. */
export function requireOneOf<T extends string>(
  flag: string,
  value: string,
  allowed: readonly T[],
): T {
  if ((allowed as readonly string[]).includes(value)) return value as T
  throw new Error(`Invalid ${flag} '${value}': expected one of ${allowed.join(', ')}`)
}

/** Parse a positive integer id, naming the argument in the error. */
export function parseId(name: string, value: string): number {
  if (!/^\d+$/.test(value) || Number(value) <= 0) {
    throw new Error(`Invalid ${name} '${value}': expected a positive integer`)
  }
  return Number(value)
}

/** Parse a TCP/UDP port (1-65535). */
export function parsePort(flag: string, value: string): number {
  if (!/^\d+$/.test(value) || Number(value) < 1 || Number(value) > 65535) {
    throw new Error(`Invalid ${flag} '${value}': expected a port between 1 and 65535`)
  }
  return Number(value)
}

export interface CreateRuleOptions {
  subjectKind: string
  subjectId: string
  direction: string
  peerKind: string
  peer: string
  action: string
  protocol?: string
  port?: string
  portEnd?: string
  description?: string
}

/** Validate `rules create` flags and build the request body. Throws on the
 * first invalid flag so nothing malformed reaches the API. */
export function buildCreateRuleBody(options: CreateRuleOptions): CreateNetworkPolicyRuleRequest {
  const portStart = options.port !== undefined ? parsePort('--port', options.port) : null
  const portEnd = options.portEnd !== undefined ? parsePort('--port-end', options.portEnd) : null
  if (portEnd !== null && portStart === null) {
    throw new Error('--port-end needs --port (the start of the range)')
  }
  if (portStart !== null && portEnd !== null && portEnd < portStart) {
    throw new Error(`--port-end ${portEnd} is below --port ${portStart}`)
  }
  const peerRef = options.peer.trim()
  if (peerRef === '') {
    throw new Error('--peer cannot be empty')
  }
  return {
    subject_kind: requireOneOf('--subject-kind', options.subjectKind, SUBJECT_KINDS),
    subject_id: parseId('--subject-id', options.subjectId),
    direction: requireOneOf('--direction', options.direction, DIRECTIONS),
    peer_kind: requireOneOf('--peer-kind', options.peerKind, PEER_KINDS),
    peer_ref: peerRef,
    action: requireOneOf('--action', options.action, ACTIONS),
    protocol:
      options.protocol !== undefined
        ? requireOneOf('--protocol', options.protocol, PROTOCOLS)
        : null,
    port_start: portStart,
    port_end: portEnd,
    description: options.description?.trim() ? options.description.trim() : null,
  }
}

/** Build the `profile set` body. An omitted flag is sent as `null`, which the
 * server reads as "use the kind's default" for that direction. */
export function buildProfileBody(options: { egress?: string; ingress?: string }): SetProfileRequest {
  return {
    egress: options.egress !== undefined ? requireOneOf('--egress', options.egress, PROFILES) : null,
    ingress:
      options.ingress !== undefined ? requireOneOf('--ingress', options.ingress, PROFILES) : null,
  }
}

/** Human form of a rule's port range: "all", "443" or "8000-8100". */
export function formatPorts(rule: Pick<NetworkPolicyRuleResponse, 'port_start' | 'port_end'>): string {
  if (rule.port_start == null) return 'all'
  if (rule.port_end == null || rule.port_end === rule.port_start) return String(rule.port_start)
  return `${rule.port_start}-${rule.port_end}`
}

export function registerNetworkPolicyCommands(program: Command): void {
  const netpol = program
    .command('network-policy')
    .alias('netpol')
    .description('Manage network policy for workloads: rules, egress/ingress profiles and the effective policy')

  const rules = netpol.command('rules').description('Manage network policy rules')

  rules
    .command('list')
    .alias('ls')
    .description('List network policy rules')
    .option('--subject-kind <kind>', `Only rules for this kind (${SUBJECT_KINDS.join(', ')})`)
    .option('--subject-id <id>', 'Only rules for this subject (environment id for deployments)')
    .option('--origin <origin>', `Only rules of this origin (${ORIGINS.join(', ')})`)
    .option('--page <n>', 'Page number (default 1)')
    .option('--page-size <n>', 'Rules per page (default 20, max 100)')
    .option('--json', 'Output in JSON format')
    .action(listRulesAction)

  rules
    .command('show <ruleId>')
    .description('Show one network policy rule')
    .option('--json', 'Output in JSON format')
    .action(showRuleAction)

  rules
    .command('create')
    .alias('add')
    .description('Create a network policy rule')
    .requiredOption('--subject-kind <kind>', `Workload kind (${SUBJECT_KINDS.join(', ')})`)
    .requiredOption('--subject-id <id>', 'Environment id for a deployment; the row id for other kinds')
    .requiredOption('--direction <direction>', `Traffic direction (${DIRECTIONS.join(', ')})`)
    .requiredOption(
      '--peer-kind <kind>',
      'Peer type: service (by id), workload (<kind>/<id>) or cidr. host rules are refused until a node runs the shared egress proxy',
    )
    .requiredOption('--peer <ref>', 'Peer reference: a service id, <kind>/<id>, or a CIDR')
    .requiredOption('--action <action>', `What to do with matching traffic (${ACTIONS.join(', ')})`)
    .option('--protocol <protocol>', `${PROTOCOLS.join(', ')} (default tcp)`)
    .option('--port <port>', 'Port, or the start of a range. Omit for every port')
    .option('--port-end <port>', 'End of the port range (needs --port)')
    .option('--description <text>', 'Why this rule exists')
    .option('--json', 'Output the created rule in JSON format')
    .action(createRuleAction)

  rules
    .command('rm <ruleId>')
    .alias('remove')
    .description('Delete a network policy rule (link rules are removed by unlinking instead)')
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation (alias for --force)')
    .action(removeRuleAction)

  netpol
    .command('effective <kind> <id>')
    .description(
      `Show the policy in force for a workload: profile, rules, inbound links and where it is enforced. <kind> is one of ${SUBJECT_KINDS.join(', ')}; <id> is the environment id for a deployment`,
    )
    .option('--json', 'Output in JSON format')
    .action(effectiveAction)

  const profile = netpol.command('profile').description("Manage a workload's egress and ingress profile")

  profile
    .command('set <kind> <id>')
    .description(
      `Set a workload's profile. Each direction is ${PROFILES.join(', ')}; a direction left out is reset to the kind's default`,
    )
    .option('--egress <profile>', `Outbound profile (${PROFILES.join(', ')}). Omit for the kind's default`)
    .option('--ingress <profile>', `Inbound profile (${PROFILES.join(', ')}). Omit for the kind's default`)
    .option('--json', 'Output in JSON format')
    .action(setProfileAction)
}

function ruleColumns(): TableColumn<NetworkPolicyRuleResponse>[] {
  return [
    { header: 'ID', key: 'id', width: 6 },
    { header: 'Subject', accessor: (r) => `${r.subject_kind}/${r.subject_id}` },
    { header: 'Dir', key: 'direction' },
    { header: 'Action', key: 'action', color: (v) => (v === 'allow' ? colors.success(v) : colors.error(v)) },
    { header: 'Peer', accessor: (r) => `${r.peer_kind}:${r.peer_ref}`, color: (v) => colors.bold(v) },
    { header: 'Proto', key: 'protocol' },
    { header: 'Ports', accessor: (r) => formatPorts(r) },
    { header: 'Origin', key: 'origin' },
    {
      header: 'Enforced',
      accessor: (r) => (r.enforceable ? 'yes' : 'NO (host rule)'),
      color: (v) => (v === 'yes' ? v : colors.warning(v)),
    },
  ]
}

function warnUnenforceable(items: NetworkPolicyRuleResponse[]): void {
  if (items.some((r) => !r.enforceable)) {
    warning(
      'Rules marked "NO (host rule)" are stored but not enforced: hostname rules need the shared egress proxy, which no node runs yet.',
    )
  }
}

interface ListRulesOptions {
  subjectKind?: string
  subjectId?: string
  origin?: string
  page?: string
  pageSize?: string
  json?: boolean
}

async function listRulesAction(options: ListRulesOptions): Promise<void> {
  const query = {
    subject_kind:
      options.subjectKind !== undefined
        ? requireOneOf('--subject-kind', options.subjectKind, SUBJECT_KINDS)
        : undefined,
    subject_id: options.subjectId !== undefined ? parseId('--subject-id', options.subjectId) : undefined,
    origin: options.origin !== undefined ? requireOneOf('--origin', options.origin, ORIGINS) : undefined,
    page: options.page !== undefined ? parseId('--page', options.page) : undefined,
    page_size: options.pageSize !== undefined ? parseId('--page-size', options.pageSize) : undefined,
  }

  await requireAuth()
  await setupClient()

  const result = await withSpinner('Fetching network policy rules...', async () => {
    const { data, error } = await listRules({ client, query })
    if (error) throw new Error(`Failed to list network policy rules: ${getErrorMessage(error)}`)
    if (!data) throw new Error('Failed to list network policy rules: empty response')
    return data
  })

  if (options.json) {
    json(result)
    return
  }

  newline()
  header(`${icons.info} Network Policy Rules (${result.items.length} of ${result.total})`)
  if (result.items.length === 0) {
    info('No network policy rules match')
    info(
      'Run: temps network-policy rules create --subject-kind sandbox --subject-id 1 --direction egress --peer-kind cidr --peer 10.0.0.0/8 --action deny',
    )
    newline()
    return
  }
  printTable(result.items, ruleColumns(), { style: 'minimal' })
  warnUnenforceable(result.items)
  if (result.total > result.page * result.page_size) {
    info(`Page ${result.page}; more with --page ${result.page + 1}`)
  }
  newline()
}

function printRule(rule: NetworkPolicyRuleResponse): void {
  keyValue('ID', rule.id)
  keyValue('Subject', `${rule.subject_kind}/${rule.subject_id}`)
  keyValue('Direction', rule.direction)
  keyValue('Action', rule.action)
  keyValue('Peer', `${rule.peer_kind}:${rule.peer_ref}`)
  keyValue('Protocol', rule.protocol)
  keyValue('Ports', formatPorts(rule))
  keyValue('Origin', rule.origin)
  keyValue('Enforced', rule.enforceable ? 'yes' : 'no (hostname rule; needs the shared egress proxy)')
  if (rule.description) keyValue('Description', rule.description)
  keyValue('Created', formatRelativeTime(rule.created_at))
}

async function showRuleAction(ruleIdArg: string, options: { json?: boolean }): Promise<void> {
  const ruleId = parseId('rule id', ruleIdArg)
  await requireAuth()
  await setupClient()

  const rule = await withSpinner('Fetching rule...', async () => {
    const { data, error } = await getRule({ client, path: { rule_id: ruleId } })
    if (error) throw new Error(`Failed to get network policy rule ${ruleId}: ${getErrorMessage(error)}`)
    if (!data) throw new Error(`Network policy rule ${ruleId} not found`)
    return data
  })

  if (options.json) {
    json(rule)
    return
  }
  newline()
  header(`${icons.info} Network Policy Rule ${rule.id}`)
  printRule(rule)
  newline()
}

async function createRuleAction(options: CreateRuleOptions & { json?: boolean }): Promise<void> {
  const body = buildCreateRuleBody(options)
  await requireAuth()
  await setupClient()

  const rule = await withSpinner('Creating network policy rule...', async () => {
    const { data, error } = await createRule({ client, body })
    if (error) {
      throw new Error(
        `Failed to create ${body.direction} rule for ${body.subject_kind}/${body.subject_id}: ${getErrorMessage(error)}`,
      )
    }
    if (!data) throw new Error('Failed to create network policy rule: empty response')
    return data
  })

  if (options.json) {
    json(rule)
    return
  }
  success(`Network policy rule ${rule.id} created`)
  printRule(rule)
  if (!rule.enforceable) warnUnenforceable([rule])
}

async function removeRuleAction(
  ruleIdArg: string,
  options: { force?: boolean; yes?: boolean },
): Promise<void> {
  const ruleId = parseId('rule id', ruleIdArg)
  await requireAuth()
  await setupClient()

  const { data: rule, error: getError } = await getRule({ client, path: { rule_id: ruleId } })
  if (getError || !rule) {
    throw new Error(
      `Network policy rule ${ruleId} not found${getError ? `: ${getErrorMessage(getError)}` : ''}`,
    )
  }
  if (rule.origin === 'link') {
    const link = rule.link_id != null ? ` ${rule.link_table ?? 'link'} ${rule.link_id}` : ''
    throw new Error(`Rule ${ruleId} comes from a link${link}; remove the link instead of the rule`)
  }

  if (!(options.force || options.yes)) {
    const confirmed = await promptConfirm({
      message: `Delete ${rule.direction} ${rule.action} rule ${ruleId} (${rule.subject_kind}/${rule.subject_id} -> ${rule.peer_kind}:${rule.peer_ref})?`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  await withSpinner('Deleting rule...', async () => {
    const { error } = await deleteRule({ client, path: { rule_id: ruleId } })
    if (error) throw new Error(`Failed to delete network policy rule ${ruleId}: ${getErrorMessage(error)}`)
  })
  success(`Network policy rule ${ruleId} deleted`)
}

function printProfile(profile: ProfileResponse): void {
  keyValue('Egress', `${profile.egress} (${profile.egress_source})`)
  keyValue('Ingress', `${profile.ingress} (${profile.ingress_source})`)
}

async function effectiveAction(kindArg: string, idArg: string, options: { json?: boolean }): Promise<void> {
  const kind = requireOneOf('kind', kindArg, SUBJECT_KINDS)
  const id = parseId('id', idArg)
  await requireAuth()
  await setupClient()

  const policy = await withSpinner('Fetching effective policy...', async () => {
    const { data, error } = await getEffectivePolicy({
      client,
      path: { subject_kind: kind, subject_id: id },
    })
    if (error) throw new Error(`Failed to get the effective policy of ${kind}/${id}: ${getErrorMessage(error)}`)
    if (!data) throw new Error(`Failed to get the effective policy of ${kind}/${id}: empty response`)
    return data
  })

  if (options.json) {
    json(policy)
    return
  }

  newline()
  header(`${icons.info} Effective network policy: ${policy.subject_kind}/${policy.subject_id}`)
  printProfile(policy.profile)
  keyValue('Enforced', policy.enforced ? 'yes' : 'no')
  keyValue(
    'Enforcing nodes',
    policy.enforcing_nodes.length > 0 ? policy.enforcing_nodes.join(', ') : 'none',
  )
  if (!policy.enforced) {
    warning('This policy is shown but not enforced on any node yet.')
  }

  newline()
  header(`Rules (${policy.rules.length})`)
  if (policy.rules.length === 0) {
    info('No rules: the profile alone decides')
  } else {
    printTable(policy.rules, ruleColumns(), { style: 'minimal' })
  }

  if (policy.inbound_links.length > 0) {
    newline()
    header(`Inbound links (${policy.inbound_links.length})`)
    info('Accepted even when ingress is restricted')
    printTable(policy.inbound_links, ruleColumns(), { style: 'minimal' })
  }
  warnUnenforceable([...policy.rules, ...policy.inbound_links])
  newline()
}

async function setProfileAction(
  kindArg: string,
  idArg: string,
  options: { egress?: string; ingress?: string; json?: boolean },
): Promise<void> {
  const kind = requireOneOf('kind', kindArg, SUBJECT_KINDS)
  const id = parseId('id', idArg)
  const body = buildProfileBody(options)
  await requireAuth()
  await setupClient()

  const profile = await withSpinner('Updating profile...', async () => {
    const { data, error } = await setProfile({
      client,
      path: { subject_kind: kind, subject_id: id },
      body,
    })
    if (error) throw new Error(`Failed to set the profile of ${kind}/${id}: ${getErrorMessage(error)}`)
    if (!data) throw new Error(`Failed to set the profile of ${kind}/${id}: empty response`)
    return data
  })

  if (options.json) {
    json(profile)
    return
  }
  success(`Profile updated for ${colors.primary(`${kind}/${id}`)}`)
  printProfile(profile)
}
