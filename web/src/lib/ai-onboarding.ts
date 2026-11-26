// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  AiHarnessCredential,
  AiHarnessState,
  AiHarnessStatusResponse,
} from '@/api/client'

export const AI_HARNESS_KEY_NAME = 'Temps AI harness'
/**
 * The CLI the console's copied commands run: the npm dist-tag of the CLI's
 * release channel (`beta` while it is a prerelease, `latest` once stable), not
 * an exact version. A server release is public before a maintainer approves its
 * CLI on npm, so an exact version would name a package npm does not serve yet;
 * the dist-tag always resolves to an installable CLI. The release moves this
 * spec when the CLI changes channel (the CLI's `channel_pinned` files in
 * `.github/releases/packages.json`). Keep it the only literal spec here.
 */
const TEMPS_CLI_PACKAGE = '@temps-sdk/cli@beta'
export const TEMPS_CLI_VERSION = TEMPS_CLI_PACKAGE.slice(
  TEMPS_CLI_PACKAGE.lastIndexOf('@') + 1
)

export type AiHarnessStatus = AiHarnessState

/** How often the page and the banner re-check a pending connection. */
export const AI_HARNESS_POLL_INTERVAL_MS = 10_000

export interface AiHarnessCommands {
  installSkill: string
  connectCli: string
  verifyIdentity: string
  verifyProjects: string
}

export interface AiHarnessKeyRequest {
  name: string
  role_type: 'admin'
}

export function buildAiHarnessKeyRequest(): AiHarnessKeyRequest {
  return {
    name: AI_HARNESS_KEY_NAME,
    role_type: 'admin',
  }
}

/**
 * The connection status the server derived, or `undefined` while it is
 * unknown (loading, or the caller cannot read API keys). The server decides,
 * not the client, because only it can see every key: the paginated key list
 * hid the dedicated key behind newer ones, and only it knows which device
 * login was approved after the dedicated key was created.
 */
export function harnessStatusOf(
  data: Pick<AiHarnessStatusResponse, 'status'> | undefined
): AiHarnessStatus | undefined {
  return data?.status
}

/** Poll while the harness is pending, stop once it is resolved either way. */
export function harnessPollInterval(
  data: Pick<AiHarnessStatusResponse, 'status'> | undefined
): number | false {
  return harnessStatusOf(data) === 'waiting'
    ? AI_HARNESS_POLL_INTERVAL_MS
    : false
}

/** What the onboarding page shows; `unknown` while the server has not answered. */
export type OnboardingHarnessStatus = AiHarnessStatus | 'unknown'

/** How often /setup/ai re-checks while the user waits on that page. */
export const ONBOARDING_POLL_INTERVAL_MS = 5_000

/**
 * The status /setup/ai renders. A key created in this visit counts as
 * waiting until the server reports it, even when the refetch after creation
 * failed. Otherwise a failed or unfinished request is `unknown`, never
 * `missing`: offering "Create" without knowing whether a key exists would
 * conflict with one that does.
 */
export function resolveOnboardingStatus(
  data: Pick<AiHarnessStatusResponse, 'status' | 'key_id'> | undefined,
  { keyJustCreated }: { keyJustCreated: boolean }
): OnboardingHarnessStatus {
  const status = harnessStatusOf(data)
  const noKeyReported =
    status === undefined || (status === 'missing' && data?.key_id == null)
  if (keyJustCreated && noKeyReported) return 'waiting'
  return status ?? 'unknown'
}

/** A key this visit created, and whether the server has reported it yet. */
export interface CreatedHarnessKey {
  secret: string
  id: number
  acknowledged: boolean
}

/**
 * Track the key this visit created against the server's answer. Until the
 * server reports that key, the page keeps treating it as waiting (even
 * through failed refetches). Once acknowledged, a later successful `missing`
 * with no key means it was removed elsewhere: forget it, so the page stops
 * showing a revoked secret and offers creation again. Returns the same
 * object when nothing changes.
 */
export function reconcileCreatedKey(
  created: CreatedHarnessKey | null,
  data: Pick<AiHarnessStatusResponse, 'status' | 'key_id'> | undefined
): CreatedHarnessKey | null {
  if (!created || !data) return created
  if (!created.acknowledged) {
    return data.key_id === created.id
      ? { ...created, acknowledged: true }
      : created
  }
  return data.status === 'missing' && data.key_id == null ? null : created
}

/** Poll /setup/ai for as long as it shows waiting, whatever the cache holds. */
export function onboardingPollInterval(
  data: Pick<AiHarnessStatusResponse, 'status' | 'key_id'> | undefined,
  options: { keyJustCreated: boolean }
): number | false {
  return resolveOnboardingStatus(data, options) === 'waiting'
    ? ONBOARDING_POLL_INTERVAL_MS
    : false
}

/**
 * Whether the confirmed "Remove pending key" may still go ahead: only while
 * the server still reports that same key as waiting. If the harness connects
 * while the confirmation is open, removing the key would revoke the
 * credential that just connected it.
 */
export function canRemovePendingKey(
  data: Pick<AiHarnessStatusResponse, 'status' | 'key_id'> | undefined,
  confirmedKeyId: number | null
): boolean {
  return (
    confirmedKeyId !== null &&
    data?.status === 'waiting' &&
    data.key_id === confirmedKeyId
  )
}

/**
 * The app-wide "Finish connecting your AI harness" banner shows only while a
 * dedicated key is waiting, never on the pages where the user resolves it,
 * and never once the user has marked the harness step done themselves.
 */
export function shouldShowHarnessPendingBanner({
  status,
  pathname,
  completedManually,
}: {
  status: AiHarnessStatus | undefined
  pathname: string
  completedManually: boolean
}): boolean {
  if (status !== 'waiting' || completedManually) return false
  return pathname !== '/setup/ai' && !pathname.startsWith('/settings/keys')
}

/** One line naming the credential that proved the harness connected. */
export function describeHarnessConnection(
  via: AiHarnessCredential | null | undefined
): string | null {
  if (!via) return null
  switch (via.kind) {
    case 'dedicated_key':
      return `Authenticated with the “${AI_HARNESS_KEY_NAME}” key`
    case 'cli_login':
      return `Authenticated through a browser-approved CLI login (key ${via.key_prefix}…)`
  }
}

function normalizeOrigin(origin: string): string {
  return origin.replace(/\/+$/, '')
}

function contextName(origin: string): string {
  try {
    return new URL(origin).hostname.replace(/[^a-z0-9-]+/gi, '-') || 'temps'
  } catch {
    return 'temps'
  }
}

/**
 * Secret-safe commands for connecting an external AI harness to Temps.
 * The API key is read without terminal echo and referenced through a shell
 * variable so the real value never appears in copied commands.
 *
 * The connect command is one `( … )` subshell of POSIX `sh` that also runs
 * unchanged in zsh, the macOS default shell:
 * - no `read -p`, which zsh parses as "read from a coprocess", so the read
 *   failed, the key stayed empty and the next line ran
 *   `login --api-key ""`, a browser login minting a different key;
 * - one compound command, so the shell has read the whole block before
 *   `read` runs; pasted line by line (no bracketed paste, e.g. macOS's
 *   bash 3.2), `read` otherwise consumed the following pasted line as the key;
 * - login runs only when a key was entered;
 * - a subshell, so the key never reaches the user's shell and the traps that
 *   restore the saved terminal settings on exit or Ctrl-C end with it.
 */
export function buildAiHarnessCommands(origin: string): AiHarnessCommands {
  const instanceOrigin = normalizeOrigin(origin)
  const context = contextName(instanceOrigin)
  const cli = `bunx @temps-sdk/cli@${TEMPS_CLI_VERSION}`

  return {
    installSkill: 'bunx skills add gotempsh/temps --skill temps',
    connectCli: [
      '(',
      '  tty_state=$(stty -g 2>/dev/null)',
      '  restore_tty() { [ -n "$tty_state" ] && stty "$tty_state" 2>/dev/null; }',
      '  trap restore_tty EXIT',
      "  trap 'restore_tty; exit 130' INT TERM HUP",
      "  printf 'Temps API key: '; stty -echo 2>/dev/null",
      '  IFS= read -r TEMPS_API_KEY; restore_tty; echo',
      '  if [ -n "$TEMPS_API_KEY" ]; then',
      `    ${cli} login ${instanceOrigin} --api-key "$TEMPS_API_KEY" --context ${context}`,
      '  else',
      "    echo 'No API key entered; the CLI was not logged in.' >&2",
      '  fi',
      ')',
    ].join('\n'),
    verifyIdentity: `${cli} --target-context ${context} whoami`,
    verifyProjects: `${cli} --target-context ${context} projects list --json`,
  }
}
