// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export function sourceArchiveUploadsSupported(
  features: { stateless?: boolean } | undefined
): boolean {
  return features?.stateless === false
}

export function persistentWorkspaceStorageSupported(
  features: { persistent_workspaces?: boolean } | undefined
): boolean {
  return features?.persistent_workspaces === true
}
