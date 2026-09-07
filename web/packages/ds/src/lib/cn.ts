// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { clsx, type ClassValue } from 'clsx'
import { twMerge } from 'tailwind-merge'

/**
 * Merge conditional class lists, letting later Tailwind utilities win over
 * earlier conflicting ones. The one class helper every op primitive uses.
 */
export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs))
}
