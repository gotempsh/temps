// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  IsolationMode,
  PluginIsolation,
  PluginIsolationResponse,
} from '@/api/client/types.gen'
import type { StatusTone } from '@temps-sdk/ds'

/** What the console says about each way a plugin can run. */
export function isolationSummary(mode: IsolationMode | undefined): {
  tone: StatusTone
  label: string
  detail: string
} {
  switch (mode) {
    case 'container':
      return {
        tone: 'ok',
        label: 'Contained',
        detail:
          'Runs in its own container with its own storage. It reaches Temps only through the permissions you granted.',
      }
    case 'separate_user':
      return {
        tone: 'ok',
        label: 'Separate user',
        detail:
          'Runs as another operating-system user through the plugin launcher, without access to Temps files.',
      }
    case 'process':
      return {
        tone: 'warn',
        label: 'Not contained',
        detail:
          'Runs as the Temps account. It can read and change whatever Temps can, whatever its permissions say.',
      }
    default:
      return {
        tone: 'idle',
        label: 'Not running',
        detail: 'This plugin is not running.',
      }
  }
}

/** The isolation a move would ask for, given what a plugin runs with now. */
export function oppositeIsolation(
  isolation: PluginIsolationResponse | undefined
): PluginIsolation {
  return isolation?.effective === 'container' ? 'process' : 'container'
}

/** The menu label of that move. */
export function isolationActionLabel(target: PluginIsolation): string {
  return target === 'container' ? 'Move to container' : 'Leave container'
}
