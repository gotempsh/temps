// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Keep an empty node selection in the request. The environment settings API
 * uses `[]` to clear an existing placement override, while `null` means the
 * field was not provided and therefore leaves the current override unchanged.
 */
export function targetNodesToPayload(targetNodes: number[]): number[] {
  return targetNodes
}

/** Keep an empty label object in the request for the same clear semantics. */
export function targetLabelsToPayload(
  targetLabels: Record<string, string>
): Record<string, string> {
  return targetLabels
}
