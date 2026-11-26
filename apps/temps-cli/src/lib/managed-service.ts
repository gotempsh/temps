// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * What `temps kv` and `temps blob` share about the instance-wide service
 * behind them (a Redis or an S3-compatible container Temps runs): its status,
 * and the container-name setting.
 */

import type { ManagedContainerNameSettings } from '../api/types.gen.js'
import { newline, header, icons, colors, info, keyValue } from '../ui/output.js'
import { ValidationError } from '../utils/errors.js'

export interface ManagedServiceStatus {
  enabled: boolean
  healthy: boolean
  docker_image?: string | null
  version?: string | null
}

export function printManagedServiceStatus(title: string, command: string, status: ManagedServiceStatus): void {
  newline()
  header(`${icons.info} ${title}`)
  keyValue('Enabled', status.enabled)
  keyValue('Healthy', status.enabled ? (status.healthy ? colors.success('yes') : colors.error('no')) : '-')
  keyValue('Image', status.docker_image ?? '-')
  keyValue('Version', status.version ?? '-')
  if (!status.enabled) info(`Not enabled on this instance. Run: temps ${command} enable`)
  newline()
}

export function printContainerNameSettings(title: string, settings: ManagedContainerNameSettings): void {
  newline()
  header(`${icons.info} ${title}`)
  keyValue('Container name', settings.container_name)
  keyValue('Default', settings.default_container_name)
  keyValue('Custom', settings.custom)
  keyValue('Required prefix', settings.required_prefix)
  keyValue('In use', settings.in_use)
  if (settings.in_use) info('The service exists, so its container name cannot change until it is disabled.')
  newline()
}

/**
 * The container-name request: a name, or `''` to reset to the default.
 * Checks the prefix the server requires when the caller knows it.
 */
export function containerNameRequest(
  options: { containerName?: string; reset?: boolean },
  requiredPrefix?: string,
): { container_name: string } {
  if (options.containerName !== undefined && options.reset) {
    throw new ValidationError('Pass --container-name or --reset, not both')
  }
  if (options.reset) return { container_name: '' }
  const name = options.containerName?.trim()
  if (!name) throw new ValidationError('Pass --container-name <name>, or --reset for the default')
  if (requiredPrefix && !name.startsWith(requiredPrefix)) {
    throw new ValidationError(`The container name must start with "${requiredPrefix}", got "${name}"`)
  }
  return { container_name: name }
}
