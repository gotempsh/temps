// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// What the console says about whether a project's addresses reach their
// audience (ADR 064 D3), and the small decisions the Make it reachable
// dialog makes from a plan and a run (D4).

import type {
  AddressReachability,
  Decision,
  EnvironmentReachability,
  JournalEntry,
  PlanOption,
  ReachabilityPlan,
  RunState,
  RunView,
  SetupField,
  TailnetPrincipal,
} from '@/api/client'

export type ReachabilityHeadline =
  | { kind: 'public'; address: string; url: string }
  /** Reachable only from a private network: `network` is `Tailscale` or
   * the range (ADR 064 D7). */
  | { kind: 'private'; address: string; url: string; network: string | null }
  | { kind: 'not_reachable'; reason: string; canFix: boolean }
  | { kind: 'unknown'; reason: string }
  | { kind: 'checking'; reason: string }

/** The address a visitor opens, as a URL. */
export function addressUrl(address: string): string {
  return /^https?:\/\//.test(address) ? address : `https://${address}`
}

/** The best address to show: reachable custom domains before generated ones. */
export function bestAddress(
  addresses: AddressReachability[]
): AddressReachability | undefined {
  const reachable = addresses.filter((a) => a.state === 'reachable')
  return (
    reachable.find((a) => a.source === 'custom_domain') ??
    reachable.find((a) => a.source === 'environment_domain') ??
    reachable[0]
  )
}

/** The header line: "Public at …" or "Not reachable yet — <reason>". */
export function reachabilityHeadline(
  reachability: EnvironmentReachability
): ReachabilityHeadline {
  const best = bestAddress(reachability.addresses)
  if (best?.audience === 'private')
    return {
      kind: 'private',
      address: best.address,
      url: addressUrl(best.address),
      network: best.network ?? null,
    }
  if (best)
    return {
      kind: 'public',
      address: best.address,
      url: addressUrl(best.address),
    }
  if (reachability.state === 'checking')
    return { kind: 'checking', reason: reachability.summary }
  if (reachability.state === 'unknown')
    return { kind: 'unknown', reason: reachability.summary }
  return {
    kind: 'not_reachable',
    reason: reachability.summary,
    canFix: (reachability.plan?.options.length ?? 0) > 0,
  }
}

/** How traffic gets in, in words. */
export function wayInLabel(wayIn: string | null | undefined): string {
  if (!wayIn) return 'No way in yet'
  if (wayIn === 'direct') return 'Direct'
  if (wayIn === 'tunnel:new') return 'A new tunnel'
  const [kind, name] = wayIn.split(':', 2)
  if (kind === 'tunnel') return `Tunnel ${name}`
  if (kind === 'private') return `Private listener on ${name}`
  if (kind === 'direct')
    return name === 'control-plane' ? 'This server' : `Straight to ${name}`
  return wayIn
}

/**
 * For the private option: the warning that its public DNS record reveals
 * the name and the internal address, shown before Create.
 */
export function privateDnsWarning(option: PlanOption): string | null {
  if (option.kind !== 'private_worker') return null
  return option.warnings.find((w) => w.includes('public DNS record')) ?? null
}

/**
 * Who an address is for, as the Domains list says it: a private one names
 * its network, `Tailscale` or the range (ADR 064 D7).
 */
export function audienceLabel(
  address: Pick<AddressReachability, 'audience' | 'network'>
): string {
  if (address.audience !== 'private') return AUDIENCE_LABELS[address.audience]
  const network = address.network
  if (!network) return 'Private'
  return network === 'Tailscale' || network === '100.64.0.0/10'
    ? 'Private: Tailscale only'
    : `Private: ${network} only`
}

export const AUDIENCE_LABELS: Record<AddressReachability['audience'], string> =
  {
    public: 'Anyone',
    private: 'My network',
    signed_in: 'Signed-in people',
    tailnet_users: 'Listed tailnet users',
    unsupported: 'Set by a newer Temps',
  }

/**
 * The run is still doing something (or Temps is recovering it after a
 * restart); keep polling it.
 */
export function isActiveRun(state: RunState): boolean {
  return state === 'running' || state === 'undoing' || state === 'recovering'
}

/**
 * Whether the run left something that undo would remove. `created` is the
 * journal's done entries; undo marks what it removes, so it shrinks.
 */
export function canUndoRun(run: RunView): boolean {
  if (isActiveRun(run.state) || run.state === 'undone') return false
  // Undo is paused at a conflict: deciding it carries undo on.
  if (run.state === 'undo_paused') return false
  if (run.journal?.some((e) => e.state === 'pending')) return false
  return run.created.length > 0
}

/**
 * The journal entries waiting for the user's decision (ADR 064 D4): while
 * going forward every one in conflict; while undo is paused, the ones undo
 * stopped at (a conflict found before writing has nothing to put back).
 */
export function conflictsToDecide(run: RunView): JournalEntry[] {
  if (run.state !== 'conflict' && run.state !== 'undo_paused') return []
  return (run.journal ?? []).filter(
    (e) => e.state === 'conflict' && (run.state === 'conflict' || !e.unwritten)
  )
}

/** What a journal entry is about, in a few words. */
export function conflictSubject(entry: JournalEntry): string {
  const intent = entry.intent
  switch (intent.type) {
    case 'dns_record':
    case 'dns_change':
      return `The ${intent.record_type} record for ${intent.hostname}`
    case 'connector':
      return `Tunnel ${intent.name}`
    case 'tailscale_serve':
      return `${intent.hostname} on ${intent.node_name}`
    case 'tailscale_service':
      return `Tailscale Service \`${intent.name}\``
    default:
      return 'This step'
  }
}

export interface ConflictChoice {
  decision: Decision
  label: string
}

/**
 * The answers to one conflict. Undo never deletes a DNS record this run did
 * not write, so a created record someone changed can only be left; nor a
 * Serve handler on a worker.
 */
export function conflictChoices(
  entry: JournalEntry,
  undoing: boolean
): ConflictChoice[] {
  const type = entry.intent.type
  if (!undoing)
    return [
      { decision: 'keep', label: 'Keep their value' },
      { decision: 'overwrite', label: 'Let Temps set it' },
    ]
  if (type === 'connector')
    return [
      { decision: 'keep', label: 'Keep the tunnel' },
      { decision: 'overwrite', label: 'Remove it anyway' },
    ]
  if (type === 'dns_record') return [{ decision: 'keep', label: 'Leave it' }]
  // ADR 065 D4: Temps never removes Serve configuration it did not write.
  if (type === 'tailscale_serve')
    return [{ decision: 'keep', label: 'Leave the worker as it is' }]
  // ADR 065 D4: a Service is deleted only when a person confirms what undo
  // showed; deleting it needs a recent password or passkey confirmation.
  if (type === 'tailscale_service')
    return [
      { decision: 'keep', label: 'Keep it' },
      { decision: 'overwrite', label: 'Delete it' },
    ]
  return [
    { decision: 'keep', label: 'Keep their value' },
    { decision: 'overwrite', label: 'Restore what was there before' },
  ]
}

/** A waiting or interrupted run carries on from its first unfinished step. */
export function canResumeRun(run: RunView): boolean {
  if (run.state !== 'waiting' && run.state !== 'interrupted') return false
  return !run.journal?.some((e) => e.state === 'pending')
}

/**
 * Whether resuming needs the Cloudflare token again: the run had not saved
 * the account yet, and the token is never stored before that.
 */
export function resumeNeedsToken(run: RunView): boolean {
  return run.steps.some(
    (s) =>
      s.kind === 'save_dns_provider' &&
      s.status !== 'done' &&
      s.status !== 'skipped'
  )
}

/**
 * Whether resuming needs the Tailscale OAuth client again (ADR 065 D2): the
 * run had not saved it, and it is never stored before that.
 */
export function resumeNeedsTailscaleCredential(run: RunView): boolean {
  return run.steps.some(
    (s) =>
      s.kind === 'save_tailscale_credential' &&
      s.status !== 'done' &&
      s.status !== 'skipped'
  )
}

/** What resuming asks for a Tailscale OAuth client, keyed as the server reads it. */
export const TAILSCALE_RESUME_FIELDS: ReadonlyArray<{
  key: string
  label: string
  secret: boolean
  required: boolean
  placeholder?: string
}> = [
  {
    key: 'tailscale_client_id',
    label: 'Tailscale OAuth client ID',
    secret: false,
    required: true,
  },
  {
    key: 'tailscale_client_secret',
    label: 'Tailscale OAuth client secret',
    secret: true,
    required: true,
  },
  {
    key: 'tailscale_tailnet',
    label: 'Tailnet',
    secret: false,
    required: false,
    placeholder: "- (the OAuth client's own tailnet)",
  },
]

/**
 * The new plan a refused apply carries (409, ADR 064 D4): the cluster
 * changed since the plan was made, so nothing was created and this is the
 * plan to review instead.
 */
export function stalePlan(error: unknown): ReachabilityPlan | null {
  if (!error || typeof error !== 'object') return null
  const plan = (error as { plan?: unknown }).plan
  if (!plan || typeof plan !== 'object') return null
  return Array.isArray((plan as { options?: unknown }).options)
    ? (plan as ReachabilityPlan)
    : null
}

/**
 * Whether the plan on screen is the one for the address typed now, so it can
 * be applied. Not while the typed address is still settling (`requested` is
 * its debounced value, which keys the plan query) or while the plan for it is
 * loading (the previous one is shown as placeholder data): applying then
 * would pair the old `plan_id` with the new address, and the server refuses
 * that as stale.
 */
export function planIsCurrent({
  typed,
  requested,
  placeholder,
}: {
  typed: string
  requested: string
  placeholder: boolean
}): boolean {
  return !placeholder && typed.trim() === requested
}

/** The option to preselect: the recommended one, else the first. */
export function defaultOption(options: PlanOption[]): PlanOption | undefined {
  return options.find((o) => o.recommended) ?? options[0]
}

/**
 * Answers for the apply request, from the form's text values. DNS provider
 * fields carry the provider id as a number; blank answers are left out.
 */
export function applyValues(
  fields: SetupField[],
  answers: Record<string, string>,
  address: string | null | undefined
): Record<string, string | number> {
  const values: Record<string, string | number> = {}
  const asked = new Set(fields.map((f) => f.key))
  if (!asked.has('address') && address) values.address = address
  for (const field of fields) {
    const answer = answers[field.key]?.trim()
    if (!answer) continue
    values[field.key] =
      field.kind.type === 'dns_provider' && /^\d+$/.test(answer)
        ? Number(answer)
        : answer
  }
  return values
}

/** The required fields still unanswered, by label. */
export function missingAnswers(
  fields: SetupField[],
  answers: Record<string, string>
): string[] {
  return fields
    .filter((f) => f.required && !answers[f.key]?.trim())
    .map((f) => f.label)
}

export interface CertificateChoices {
  /** HTTP-01 is listed: always, except for a wildcard. */
  showHttp01: boolean
  /** The choice labelled Recommended, if any. */
  recommended: 'tunnel' | 'http-01' | 'dns-01' | null
  /** Why HTTP-01 may not work here. */
  http01Note: string | null
}

/**
 * Which certificate methods Add Domain offers. HTTP-01 is only recommended
 * when a direct ingress takes port 80 (ADR 064 D4). It stays listed when
 * Temps sees none: port forwarding or a load balancer can bring port 80 to
 * a server that listens elsewhere, which Temps cannot see from inside.
 */
export function certificateChoices({
  coveringTunnels,
  wildcard,
  http01Available,
}: {
  coveringTunnels: number
  wildcard: boolean
  http01Available: boolean | undefined
}): CertificateChoices {
  const recommended =
    coveringTunnels > 0
      ? 'tunnel'
      : wildcard
        ? 'dns-01'
        : http01Available === true
          ? 'http-01'
          : null
  return {
    showHttp01: !wildcard,
    recommended,
    http01Note:
      !wildcard && coveringTunnels === 0 && http01Available === false
        ? 'Temps does not see a public listener on port 80. HTTP-01 works only if port 80 reaches this server, for example through port forwarding; otherwise use DNS-01 or a tunnel.'
        : null,
  }
}

/** The most entries one address may list, as the server bounds it. */
export const MAX_PRINCIPALS = 64

/**
 * Who a `tailnet_users` address admits, from what the user typed: entries
 * separated by commas, spaces or new lines; `tag:<name>` is a tag and
 * `<user>@<domain>` a login (ADR 065 D6). Values are ASCII-lower-cased,
 * sorted and deduplicated the way the server stores them; anything that is
 * neither is returned in `invalid` rather than guessed at. The server
 * checks the list again.
 */
export function parsePrincipals(text: string): {
  principals: TailnetPrincipal[]
  invalid: string[]
} {
  const seen = new Set<string>()
  const principals: TailnetPrincipal[] = []
  const invalid: string[] = []
  for (const entry of text.split(/[\s,]+/)) {
    if (!entry) continue
    const value = entry.replace(/[A-Z]/g, (c) => c.toLowerCase())
    const principal = principalOf(value)
    if (!principal) {
      invalid.push(entry)
      continue
    }
    const key = `${principal.kind}:${principal.value}`
    if (seen.has(key)) continue
    seen.add(key)
    principals.push(principal)
  }
  principals.sort((a, b) =>
    a.kind === b.kind
      ? a.value < b.value
        ? -1
        : a.value > b.value
          ? 1
          : 0
      : a.kind === 'login'
        ? -1
        : 1
  )
  return { principals, invalid }
}

function principalOf(value: string): TailnetPrincipal | null {
  if (value.startsWith('tag:')) {
    const name = value.slice(4)
    return name.length <= 64 && /^[a-z0-9][a-z0-9-]*$/.test(name)
      ? { kind: 'tag', value }
      : null
  }
  const at = value.indexOf('@')
  const ok =
    value.length <= 320 &&
    at > 0 &&
    at < value.length - 1 &&
    // eslint-disable-next-line no-control-regex
    !/[\u0000-\u001f\u007f]/.test(value)
  return ok ? { kind: 'login', value } : null
}
