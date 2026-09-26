// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** An untouched selection follows available environments; [] is intentional. */
export function resolveEnvironmentSelection(
  selection: number[] | undefined,
  environments: ReadonlyArray<{ id: number }> | undefined
): number[] {
  return selection ?? environments?.map((environment) => environment.id) ?? []
}
