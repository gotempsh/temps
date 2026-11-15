// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** The slice of the chat's sandbox status the connection controls read. */
export interface SandboxConnectionStatus {
  linked?: unknown
  connected_projects?: readonly number[]
}

export type SandboxConnectionState = 'no-sandbox' | 'connected' | 'available'

/** Whether the chat's sandbox can use, or already uses, a project's databases. */
export function connectionState(
  status: SandboxConnectionStatus | undefined,
  projectId: number
): SandboxConnectionState {
  if (!status?.linked) return 'no-sandbox'
  return status.connected_projects?.includes(projectId)
    ? 'connected'
    : 'available'
}

export const CONNECT_LABEL = 'Connect sandbox'
export const DISCONNECT_LABEL = 'Disconnect sandbox'

export function connectionLabel(state: SandboxConnectionState): string {
  return state === 'connected' ? DISCONNECT_LABEL : CONNECT_LABEL
}

/** What the control does, shown as its tooltip. */
export function connectionTitle(state: SandboxConnectionState): string {
  switch (state) {
    case 'connected':
      return "The sandbox uses this project's database. Disconnect to cut its access."
    case 'available':
      return "Connect to this project's databases: the sandbox gets a database of its own and credentials for it alone. It never sees production data or credentials."
    case 'no-sandbox':
      return 'Link a sandbox to this chat first, then connect it to this project’s databases.'
  }
}

/** The slice of a connect result the confirmation reads. */
export interface ConnectionResult {
  variable_names: readonly string[]
  connected_service_ids: readonly number[]
  skipped_service_ids: readonly number[]
}

function plural(count: number, word: string): string {
  return `${count} ${word}${count === 1 ? '' : 's'}`
}

/** A sentence on what the sandbox can now do, with variable names only. */
export function connectedSummary(result: ConnectionResult): string {
  const databases = plural(result.connected_service_ids.length, 'service')
  const names = result.variable_names.filter((name) =>
    ['POSTGRES_URL', 'POSTGRES_HOST', 'POSTGRES_DB'].includes(name)
  )
  const shown = names.length > 0 ? ` (${names.join(', ')})` : ''
  const skipped =
    result.skipped_service_ids.length > 0
      ? ` ${plural(result.skipped_service_ids.length, 'linked service')} of another type cannot be used from the sandbox yet.`
      : ''
  return `Commands in the sandbox now get credentials of its own on ${databases}${shown}.${skipped}`
}
