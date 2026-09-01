// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { DropFile } from './drop-archive'

// File objects cannot be encoded into a URL and large folders should not be
// copied into browser history state. Keep the selection in memory for the
// single navigation from the project-list empty state to `/drop`.
let pendingDropFiles: DropFile[] | null = null

export function handOffDropFiles(files: DropFile[]): void {
  pendingDropFiles = files
}

export function consumeDropFilesHandoff(): DropFile[] | null {
  const files = pendingDropFiles
  pendingDropFiles = null
  return files
}
