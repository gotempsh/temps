// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Shield modes and the helpers the settings forms and the Shield tab share
 * (ADR 066 D1). The type mirrors the generated `ShieldMode` so this module
 * stays usable from tests without the API client.
 */
export type ShieldModeValue = 'off' | 'log' | 'selective' | 'attack'

export const SHIELD_MODES: ReadonlyArray<{
  value: ShieldModeValue
  label: string
  description: string
}> = [
  {
    value: 'off',
    label: 'Off',
    description: 'Shield does nothing; every request passes.',
  },
  {
    value: 'log',
    label: 'Log',
    description:
      'Verdicts are recorded; nothing is blocked or challenged. The default.',
  },
  {
    value: 'selective',
    label: 'Selective',
    description:
      'Block verdicts get a 403 and challenge verdicts a challenge; everyone else passes.',
  },
  {
    value: 'attack',
    label: 'Attack',
    description: 'As selective, and every other visitor is challenged too.',
  },
]

export function shieldModeLabel(mode: ShieldModeValue | null | undefined) {
  return SHIELD_MODES.find((entry) => entry.value === mode)?.label ?? 'Log'
}

export function shieldModeDescription(
  mode: ShieldModeValue | null | undefined
) {
  return SHIELD_MODES.find((entry) => entry.value === mode)?.description ?? ''
}

/** Whether the mode acts on verdicts rather than only recording them. */
export function shieldModeEnforces(mode: ShieldModeValue | null | undefined) {
  return mode === 'selective' || mode === 'attack'
}

/** Select value for an environment's mode: a mode, or inherit the project's. */
export type EnvironmentShieldModeSelect = 'inherit' | ShieldModeValue

export function environmentShieldModeToSelect(
  value: ShieldModeValue | null | undefined
): EnvironmentShieldModeSelect {
  return value ?? 'inherit'
}

/** `null` clears the environment's mode so it follows the project. */
export function environmentShieldModeToPayload(
  value: EnvironmentShieldModeSelect
): ShieldModeValue | null {
  return value === 'inherit' ? null : value
}

/** The server's limit (`temps_shield::MAX_EXEMPT_PATHS`). */
export const MAX_SHIELD_EXEMPT_PATHS = 64

/** One pattern per line; blank lines are dropped. */
export function parseExemptPathsText(text: string): string[] {
  return text
    .split('\n')
    .map((line) => line.trim())
    .filter((line) => line.length > 0)
}

export function exemptPathsToText(paths: string[] | null | undefined) {
  return (paths ?? []).join('\n')
}

/**
 * The same rules the server applies, so a typo is caught next to the field
 * instead of after a round trip. Returns the first problem, or `null`.
 */
export function validateExemptPaths(paths: string[]): string | null {
  if (paths.length > MAX_SHIELD_EXEMPT_PATHS) {
    return `At most ${MAX_SHIELD_EXEMPT_PATHS} exempt paths are allowed.`
  }
  for (const path of paths) {
    if (!path.startsWith('/')) {
      return `"${path}" must start with "/".`
    }
    const star = path.indexOf('*')
    if (star !== -1 && (star !== path.length - 1 || !path.endsWith('/*'))) {
      return `"${path}" may only end in "/*"; no other wildcards are allowed.`
    }
  }
  return null
}

export type ShieldWindowValue = '1h' | '24h' | '7d'

export const SHIELD_WINDOWS: ReadonlyArray<{
  value: ShieldWindowValue
  label: string
}> = [
  { value: '1h', label: 'Last hour' },
  { value: '24h', label: 'Last 24 hours' },
  { value: '7d', label: 'Last 7 days' },
]

export function formatSolveMs(ms: number | null | undefined): string {
  if (ms === null || ms === undefined) return '—'
  if (ms < 1000) return `${Math.round(ms)} ms`
  return `${(ms / 1000).toFixed(1)} s`
}

export type ShieldBadgeVariant =
  'destructive-subtle' | 'warning-subtle' | 'success-subtle' | 'neutral-subtle'

/** Badge tone for a verdict or entry verdict; the label always says which. */
export function shieldVerdictBadge(verdict: string): ShieldBadgeVariant {
  switch (verdict) {
    case 'block':
      return 'destructive-subtle'
    case 'challenge':
      return 'warning-subtle'
    case 'allow':
      return 'success-subtle'
    default:
      return 'neutral-subtle'
  }
}

const EVENT_KIND_LABELS: Record<string, string> = {
  verdict: 'Verdict',
  challenge_issued: 'Challenge issued',
  challenge_solved: 'Challenge solved',
  challenge_failed: 'Challenge failed',
}

export function shieldEventKindLabel(kind: string): string {
  return EVENT_KIND_LABELS[kind] ?? kind
}

/**
 * Text for a `datetime-local` input from an ISO timestamp, in local time,
 * and back. Empty means no value.
 */
export function isoToLocalInput(iso: string | null | undefined): string {
  if (!iso) return ''
  const date = new Date(iso)
  if (Number.isNaN(date.getTime())) return ''
  const offset = date.getTimezoneOffset() * 60_000
  return new Date(date.getTime() - offset).toISOString().slice(0, 16)
}

export function localInputToIso(value: string): string | null {
  if (!value) return null
  const date = new Date(value)
  return Number.isNaN(date.getTime()) ? null : date.toISOString()
}
