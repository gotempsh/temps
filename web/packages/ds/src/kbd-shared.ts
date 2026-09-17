// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Platform-aware modifier. Pass '⌘' in `keys` and it becomes Ctrl off macOS. */
export const IS_MAC =
  typeof navigator !== 'undefined' &&
  /Mac|iPhone|iPad/.test(navigator.userAgent)

export const MOD = IS_MAC ? '⌘' : 'Ctrl'
