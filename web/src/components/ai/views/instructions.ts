// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { fileURLToPath } from 'node:url'
import { viewCatalog } from './catalog'

// For the generator script and its test only: this reads a path in the
// repository, so nothing the console bundles imports it.

/**
 * The file the server compiles into the assistant's instructions. It is
 * generated from the catalog, so the components the model is told about are
 * the ones the console can draw.
 */
export const ASSISTANT_VIEWS_FILE = fileURLToPath(
  new URL(
    '../../../../../crates/temps-ai-chat/src/providers/assistant_views.md',
    import.meta.url
  )
)

/** The file's contents, as the catalog generates them. */
export function assistantViewsInstructions(): string {
  return `${viewCatalog.prompt().trim()}\n`
}
