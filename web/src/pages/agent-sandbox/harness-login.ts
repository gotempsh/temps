// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { z } from 'zod'

/**
 * Presentation helpers for the console sign-in ("Sign in with Claude").
 * The server owns the state machine; these only turn its phases into words.
 */

/** The server refuses codes longer than this (bytes; the alphabet is ASCII). */
export const MAX_LOGIN_CODE_LENGTH = 2048

/**
 * The pasted authorization code. Mirrors the server's rules so an invalid
 * paste is explained in the form rather than after a round trip: non-empty,
 * at most 2 KiB, visible ASCII only (a space or line break could become a
 * second answer to the CLI's prompt).
 */
export const loginCodeSchema = z.object({
  code: z
    .string()
    .trim()
    .min(1, 'Paste the code shown after approving access.')
    .max(
      MAX_LOGIN_CODE_LENGTH,
      `The code is longer than ${MAX_LOGIN_CODE_LENGTH} characters. Copy only the code shown on the page.`
    )
    .regex(
      /^[\x21-\x7e]+$/,
      'The code may only contain visible ASCII characters, without spaces or line breaks.'
    ),
})

export type LoginCodeFormValues = z.infer<typeof loginCodeSchema>

export type LoginPhase =
  'starting' | 'waiting' | 'verifying' | 'succeeded' | 'failed' | 'cancelled'

const TERMINAL_PHASES: ReadonlySet<string> = new Set([
  'succeeded',
  'failed',
  'cancelled',
])

/** True once the sign-in can no longer change state. */
export function isLoginTerminal(phase: string | undefined | null): boolean {
  return phase != null && TERMINAL_PHASES.has(phase)
}

/** True while a CLI process is running on the server for this sign-in. */
export function isLoginActive(phase: string | undefined | null): boolean {
  return phase != null && !TERMINAL_PHASES.has(phase)
}

/** `m:ss`, clamped at zero. */
export function formatElapsed(seconds: number): string {
  const whole = Math.max(0, Math.floor(seconds))
  const minutes = Math.floor(whole / 60)
  const rest = whole % 60
  return `${minutes}:${rest.toString().padStart(2, '0')}`
}

/** Short heading for each phase. */
export function loginPhaseTitle(phase: string, providerName: string): string {
  switch (phase) {
    case 'starting':
      return `Starting ${providerName}…`
    case 'waiting':
      return 'Waiting for you'
    case 'verifying':
      return 'Verifying the sign-in…'
    case 'succeeded':
      return 'Signed in'
    case 'failed':
      return 'Sign-in failed'
    case 'cancelled':
      return 'Sign-in cancelled'
    default:
      return 'Signing in…'
  }
}

/** Label of the sign-in button, e.g. "Sign in with Claude". */
export function loginButtonLabel(providerName: string): string {
  const short = providerName.split(/[\s(]/)[0] || providerName
  return `Sign in with ${short}`
}

/** Title of the server's 409 problem when a sign-in is already running. */
const ALREADY_IN_PROGRESS_TITLE = 'Sign-in Already In Progress'

/**
 * True when an API error is the server's "already in progress" conflict. The
 * problem body carries a title but not always a status, so match the title.
 */
export function isSignInConflict(error: unknown): boolean {
  if (typeof error !== 'object' || error === null) return false
  const problem = error as { status?: unknown; title?: unknown }
  return problem.title === ALREADY_IN_PROGRESS_TITLE || problem.status === 409
}
