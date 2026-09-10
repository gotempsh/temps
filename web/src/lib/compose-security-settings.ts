// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export interface ComposeSecuritySettings {
  relaxedCapabilityServices: string[]
  unsandboxedServices: string[]
}

/**
 * Apply a per-service sandbox choice while keeping the two security modes
 * mutually exclusive. Disabling the sandbox makes the narrower capability
 * exception redundant, so it is removed atomically in the same settings save.
 */
export function withComposeSandboxDisabled(
  relaxedCapabilityServices: string[],
  unsandboxedServices: string[],
  serviceName: string,
  disabled: boolean
): ComposeSecuritySettings {
  return {
    relaxedCapabilityServices: disabled
      ? relaxedCapabilityServices.filter((name) => name !== serviceName)
      : relaxedCapabilityServices,
    unsandboxedServices: disabled
      ? Array.from(new Set([...unsandboxedServices, serviceName]))
      : unsandboxedServices.filter((name) => name !== serviceName),
  }
}
