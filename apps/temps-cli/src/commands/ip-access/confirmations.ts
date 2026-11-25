// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The two confirmations the server asks for before it saves an instance-wide
 * IP block rule: a range wider than /8 (IPv4) or /16 (IPv6), and a rule that
 * matches the address the request came from. Each is refused with a 409
 * problem whose title names the case, and saved when the request carries the
 * matching field. The console asks in a dialog; the CLI takes a flag, or asks
 * on a terminal when the flag was not given.
 */

import { getErrorMessage } from '../../lib/api-client.js'
import { withSpinner } from '../../ui/spinner.js'
import { promptConfirm } from '../../ui/prompts.js'

/** What a refused IP block rule save is waiting for the operator to confirm. */
export type IpRuleConfirmation = 'broad_range' | 'lockout'

/** The request fields that answer the confirmations. */
export interface IpRuleConfirmationFields {
  confirm_broad_range: boolean
  acknowledge_lockout: boolean
}

interface ConfirmationInfo {
  title: string
  field: keyof IpRuleConfirmationFields
  flag: string
  question: string
}

const CONFIRMATIONS: Record<IpRuleConfirmation, ConfirmationInfo> = {
  broad_range: {
    title: 'Block Range Needs Confirmation',
    field: 'confirm_broad_range',
    flag: '--confirm-broad-range',
    question: 'Block this much of the internet?',
  },
  lockout: {
    title: 'Rule Would Lock You Out',
    field: 'acknowledge_lockout',
    flag: '--acknowledge-lockout',
    question: 'This rule would lock you out, the console included. Save it anyway?',
  },
}

/**
 * Which confirmation a refused save is waiting for. Other conflicts (a
 * duplicate address, a rule changed by another request) are 409 too, so the
 * problem title decides.
 */
export function ipRuleConfirmation(error: unknown, status: number | undefined): IpRuleConfirmation | null {
  if (status !== 409 || !error || typeof error !== 'object') return null
  const title = (error as { title?: unknown }).title
  for (const [kind, info] of Object.entries(CONFIRMATIONS) as Array<[IpRuleConfirmation, ConfirmationInfo]>) {
    if (title === info.title) return kind
  }
  return null
}

/** The CLI flag that answers a confirmation. */
export function confirmationFlag(confirmation: IpRuleConfirmation): string {
  return CONFIRMATIONS[confirmation].flag
}

/**
 * The server's explanation with its API wording ("resubmit with
 * confirm_broad_range=true ...") replaced by the CLI flag that does it.
 */
export function confirmationMessage(confirmation: IpRuleConfirmation, error: unknown): string {
  return `${refusalReason(error)}. Re-run with ${confirmationFlag(confirmation)} if that is intended.`
}

/** The server's explanation without its "resubmit with <field>=true" advice. */
function refusalReason(error: unknown): string {
  return getErrorMessage(error).replace(/;\s*resubmit with \w+=true\b.*$/s, '')
}

export interface SaveResult<T> {
  data?: T
  error?: unknown
  response?: Response
}

export interface ConfirmationOptions {
  confirmBroadRange?: boolean
  acknowledgeLockout?: boolean
  /** Ask on the terminal when a confirmation is missing. `--yes` turns this off. */
  interactive: boolean
}

/** Injected in tests; the terminal prompt otherwise. */
export type Ask = (question: string) => Promise<boolean>

const askOnTerminal: Ask = (question) => promptConfirm({ message: question, default: false })

/**
 * Send a rule save, answering the server's confirmations. A confirmation is
 * given only by its flag or, on an interactive terminal, by the operator
 * saying yes to the question; `--yes` never gives one, so a script cannot
 * acknowledge a lockout by accident. Returns `undefined` when the operator
 * declined.
 */
export async function saveWithConfirmations<T>(
  progress: string,
  send: (fields: IpRuleConfirmationFields) => Promise<SaveResult<T>>,
  options: ConfirmationOptions,
  ask: Ask = askOnTerminal
): Promise<{ saved: T | undefined } | undefined> {
  const fields: IpRuleConfirmationFields = {
    confirm_broad_range: options.confirmBroadRange === true,
    acknowledge_lockout: options.acknowledgeLockout === true,
  }
  // Each confirmation is asked at most once, so this ends after three sends.
  for (;;) {
    const { data, error, response } = await withSpinner(progress, () => send({ ...fields }))
    if (!error) return { saved: data }
    const confirmation = ipRuleConfirmation(error, response?.status)
    if (confirmation === null) throw new Error(getErrorMessage(error))
    const info = CONFIRMATIONS[confirmation]
    if (fields[info.field] || !options.interactive || !process.stdin.isTTY) {
      throw new Error(confirmationMessage(confirmation, error))
    }
    console.error(`${refusalReason(error)}.`)
    if (!(await ask(info.question))) return undefined
    fields[info.field] = true
  }
}
