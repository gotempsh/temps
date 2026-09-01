// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { GettingStartedItem } from '@/hooks/useGettingStarted'

export function nextIncompleteGettingStartedItem(
  items: GettingStartedItem[]
): GettingStartedItem | undefined {
  return items.find((item) => !item.done)
}

export function firstIncompleteGettingStartedIndex(
  items: GettingStartedItem[]
): number {
  return items.findIndex((item) => !item.done)
}
