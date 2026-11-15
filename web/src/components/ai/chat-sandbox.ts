// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Tool the assistant calls to ask the user to link a sandbox. */
export const SANDBOX_OFFER_TOOL_NAME = 'temps_offer_sandbox'

/** Tool the assistant calls to create a sandbox for the chat itself. */
export const SANDBOX_CREATE_TOOL_NAME = 'temps_create_sandbox'

/** Application workspaces are managed from their application, never linked. */
const APPLICATION_WORKSPACE_PREFIX = 'ai-application:'

export function isSandboxOfferTool(name: string): boolean {
  return (
    name === SANDBOX_OFFER_TOOL_NAME ||
    name.endsWith(`__${SANDBOX_OFFER_TOOL_NAME}`)
  )
}

export function isSandboxCreateTool(name: string): boolean {
  return (
    name === SANDBOX_CREATE_TOOL_NAME ||
    name.endsWith(`__${SANDBOX_CREATE_TOOL_NAME}`)
  )
}

const SANDBOX_SCOPED_TOOL_NAMES = [
  'sandbox_exec',
  'sandbox_read_file',
  'sandbox_write_file',
  'sandbox_preview',
]

/**
 * The tools that run in the chat's linked sandbox. They can wake a sleeping
 * sandbox or unlink one whose container is gone, so the sandbox control
 * refreshes after each.
 */
export function isSandboxScopedTool(name: string): boolean {
  return SANDBOX_SCOPED_TOOL_NAMES.some(
    (scoped) => name === scoped || name.endsWith(`__${scoped}`)
  )
}

/** The slice of a sandbox the picker needs. */
export interface LinkableSandboxInput {
  id: string
  name: string
  status: string
  agent_run_id?: number | null
}

/**
 * The sandboxes a chat may be linked to. The server refuses the rest, so
 * offering them would only produce an error after the click.
 */
export function linkableSandboxes<T extends LinkableSandboxInput>(
  sandboxes: readonly T[]
): T[] {
  return sandboxes.filter(
    (sandbox) =>
      sandbox.status !== 'destroyed' &&
      sandbox.agent_run_id == null &&
      !sandbox.name.startsWith(APPLICATION_WORKSPACE_PREFIX)
  )
}

/** The slice of the sandbox status the control needs. */
export interface ChatSandboxInput {
  configured: boolean
  linked?: { name: string } | null
}

/** Short label for the composer-row button. */
export function sandboxButtonLabel(
  status: ChatSandboxInput | undefined
): string {
  if (!status) return 'Sandbox'
  if (status.linked) return status.linked.name
  return 'Sandbox'
}

/** What the assistant said it wants the sandbox for, from the tool arguments. */
export function offerReason(toolArguments: string): string | null {
  try {
    const parsed: unknown = JSON.parse(toolArguments)
    const reason =
      parsed && typeof parsed === 'object'
        ? (parsed as { reason?: unknown }).reason
        : null
    if (typeof reason !== 'string') return null
    const trimmed = reason.trim().replace(/[.\s]+$/, '')
    return trimmed.length > 0 ? `${trimmed}.` : null
  } catch {
    return null
  }
}
