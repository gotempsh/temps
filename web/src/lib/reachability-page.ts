// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// The *Make it reachable* page's state (ADR 069): what its URL says, which
// audience and gate are chosen given what the backend says is possible,
// and which run to point the user back to.

import type {
  AddressReachability,
  AudienceChoice,
  PlanBlocked,
  PlanOption,
  PlanOptionKind,
  RunState,
  RunView,
} from '@/api/client'
import { isActiveRun, type SignInGate } from '@/lib/reachability'

/** The audiences a user can ask for (ADR 064 D5, ADR 065 D6, D7). */
export type PageAudience = 'public' | 'signed_in' | 'private' | 'tailnet_users'

export const PAGE_AUDIENCES: readonly PageAudience[] = [
  'public',
  'signed_in',
  'private',
  'tailnet_users',
]

const GATES: readonly SignInGate[] = ['temps', 'cloudflare_access']

const OPTION_KINDS: readonly PlanOptionKind[] = [
  'existing_tunnel',
  'direct_managed_dns',
  'new_tunnel',
  'new_tunnel_with_token',
  'direct_manual_dns',
  'private_worker',
  'tailscale_serve',
  'tailscale_service',
  'tailscale_service_with_credential',
  'tunnel_environment_hosts',
]

/** What the page's query string says (ADR 069 D2). */
export interface ReachabilityPageParams {
  audience: PageAudience | null
  gate: SignInGate | null
  /** The hostname to plan for, as typed; `''` when none. */
  address: string
  /** A plan option to preselect when the plan offers it. */
  way: PlanOptionKind | null
  /** The run to show instead of the form. */
  run: number | null
}

function oneOf<T extends string>(
  value: string | null,
  allowed: readonly T[]
): T | null {
  return value !== null && (allowed as readonly string[]).includes(value)
    ? (value as T)
    : null
}

/** The page's state from its query string. Unknown values are ignored. */
export function readPageParams(
  search: URLSearchParams
): ReachabilityPageParams {
  const audience = oneOf(search.get('audience'), PAGE_AUDIENCES)
  const run = Number(search.get('run'))
  return {
    audience,
    gate: audience === 'signed_in' ? oneOf(search.get('gate'), GATES) : null,
    address: (search.get('address') ?? '').trim(),
    way: oneOf(search.get('way'), OPTION_KINDS),
    run: Number.isSafeInteger(run) && run > 0 ? run : null,
  }
}

/**
 * The query string with `change` applied. A gate only travels with
 * `signed_in`; an empty address, way or run is removed. Parameters the page
 * does not own are kept.
 */
export function writePageParams(
  current: URLSearchParams,
  change: Partial<ReachabilityPageParams>
): URLSearchParams {
  const next = new URLSearchParams(current)
  const set = (key: string, value: string | number | null | undefined) => {
    if (value === undefined) return
    if (value === null || value === '') next.delete(key)
    else next.set(key, String(value))
  }
  set('audience', change.audience)
  set('gate', change.gate)
  if (change.address !== undefined) set('address', change.address.trim())
  set('way', change.way)
  set('run', change.run)
  if (next.get('audience') !== 'signed_in') next.delete('gate')
  return next
}

/** The page's URL for an environment (ADR 069 D1). */
export function reachabilityPath(
  projectSlug: string,
  environmentId: number,
  params: Partial<ReachabilityPageParams> = {}
): string {
  const query = writePageParams(new URLSearchParams(), params).toString()
  const path = `/projects/${encodeURIComponent(projectSlug)}/environments/${environmentId}/reachability`
  return query ? `${path}?${query}` : path
}

/**
 * The environment the route names: by id, as Temps' own links do, else by
 * name or slug, ignoring case, for a hand-typed URL.
 */
export function resolveEnvironmentParam<
  T extends { id: number; name: string; slug?: string | null },
>(value: string | undefined, environments: T[]): T | undefined {
  if (!value) return undefined
  const trimmed = value.trim()
  if (/^\d+$/.test(trimmed)) {
    const byId = environments.find((e) => e.id === Number(trimmed))
    if (byId) return byId
  }
  const wanted = trimmed.toLowerCase()
  return environments.find(
    (e) =>
      e.name.trim().toLowerCase() === wanted ||
      (e.slug ?? '').trim().toLowerCase() === wanted
  )
}

/** One audience card: its entries (one per gate for `signed_in`). */
export interface AudienceCard {
  audience: PageAudience
  available: boolean
  /** Why it cannot work, when no entry is available. */
  blocked: PlanBlocked | null
  entries: AudienceChoice[]
}

/** The cards in the order the backend lists audiences. */
export function audienceCards(choices: AudienceChoice[]): AudienceCard[] {
  const cards: AudienceCard[] = []
  for (const choice of choices) {
    const audience = oneOf(choice.audience, PAGE_AUDIENCES)
    if (!audience) continue
    let card = cards.find((c) => c.audience === audience)
    if (!card) {
      card = { audience, available: false, blocked: null, entries: [] }
      cards.push(card)
    }
    card.entries.push(choice)
  }
  for (const card of cards) {
    card.available = card.entries.some((e) => e.available)
    card.blocked = card.available
      ? null
      : (card.entries.find((e) => e.blocked)?.blocked ?? null)
  }
  return cards
}

/** What the page shows as chosen, given what is possible. */
export interface Selection {
  audience: PageAudience | null
  gate: SignInGate | null
  /** The choices entry the plan is made for. */
  choice: AudienceChoice | null
  /**
   * What the URL asked for when it cannot work, so the page can say why
   * instead of silently choosing something else.
   */
  refused: AudienceCard | null
}

/**
 * The audience and gate to plan for. What the URL asks for, when it can
 * work; else the first audience that can, and the first gate of it that
 * can. Nothing while no audience can.
 */
export function selectAudience(
  cards: AudienceCard[],
  requested: { audience: PageAudience | null; gate: SignInGate | null }
): Selection {
  const asked = cards.find((c) => c.audience === requested.audience)
  const refused = asked && !asked.available ? asked : null
  const card = asked?.available ? asked : cards.find((c) => c.available)
  if (!card) return { audience: null, gate: null, choice: null, refused }
  const usable = card.entries.filter((e) => e.available)
  const entry =
    (card.audience === 'signed_in' && requested.audience === 'signed_in'
      ? usable.find((e) => e.gate === requested.gate)
      : undefined) ?? usable[0]
  const gate =
    card.audience === 'signed_in'
      ? (oneOf(entry?.gate ?? null, GATES) ?? 'temps')
      : null
  return { audience: card.audience, gate, choice: entry ?? null, refused }
}

/** Runs that are not over: they can still be watched, resumed or decided. */
const UNFINISHED: readonly RunState[] = [
  'running',
  'undoing',
  'recovering',
  'waiting',
  'interrupted',
  'conflict',
  'undo_paused',
]

export function isUnfinishedRun(state: RunState): boolean {
  return UNFINISHED.includes(state)
}

/**
 * The run to point back to (ADR 069 D5): the environment's newest run,
 * when it is not over. Runs come newest first.
 */
export function unfinishedRun(
  runs: RunView[] | undefined
): RunView | undefined {
  const newest = runs?.[0]
  return newest && isUnfinishedRun(newest.state) ? newest : undefined
}

/** How often to look at a run: often while it acts, slowly while it waits. */
export function runPollInterval(state: RunState | undefined): number | false {
  if (!state || isActiveRun(state)) return 2_000
  return state === 'waiting' ? 15_000 : false
}

/** A run's state in words, for links back to it. */
export const RUN_STATE_LABELS: Record<RunState, string> = {
  running: 'In progress',
  succeeded: 'Done',
  waiting: 'Waiting for the address to answer',
  failed: 'Failed',
  interrupted: 'Interrupted',
  recovering: 'Recovering',
  conflict: 'Needs your decision',
  undoing: 'Undoing',
  undo_paused: 'Undo needs your decision',
  undone: 'Undone',
}

/**
 * How a way in handles DNS, in words, from what the plan's steps do. Steps
 * the console does not know leave the answer to the steps themselves.
 */
export function dnsHandling(
  option: Pick<PlanOption, 'kind' | 'way_in' | 'steps'>
): string {
  const kinds = new Set<string>(option.steps.map((s) => s.kind))
  if (kinds.has('show_dns_record'))
    return 'You add one DNS record; the plan shows exactly which.'
  if (kinds.has('set_dns_record') || kinds.has('manage_domain'))
    return 'Temps writes the DNS record with your DNS provider.'
  if (option.way_in.startsWith('tunnel:'))
    return 'The tunnel publishes the DNS record in Cloudflare.'
  if (option.kind.startsWith('tailscale_'))
    return 'No DNS record: the name resolves inside your tailnet.'
  return 'No DNS change.'
}

/**
 * What a plan leaves as it is (ADR 069 D4): the environment's other
 * addresses, each with who it is for. The address being planned is not
 * listed: the plan's steps say what happens to it.
 */
export function addressesLeftAlone(
  addresses: Pick<AddressReachability, 'address' | 'audience'>[],
  planned: string | null | undefined
): Pick<AddressReachability, 'address' | 'audience'>[] {
  const target = (planned ?? '').trim().toLowerCase()
  return addresses.filter((a) => a.address.toLowerCase() !== target)
}
