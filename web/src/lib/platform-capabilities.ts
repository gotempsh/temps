// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export function sourceArchiveUploadsSupported(
  features: { stateless?: boolean } | undefined
): boolean {
  // Older servers omit this capability. Only an explicit restriction disables
  // the action; the API remains authoritative if capability discovery fails.
  return features?.stateless !== true
}

export function persistentWorkspaceStorageSupported(
  features: { stateless?: boolean; persistent_workspaces?: boolean } | undefined
): boolean {
  return (
    features?.stateless !== true && features?.persistent_workspaces !== false
  )
}
