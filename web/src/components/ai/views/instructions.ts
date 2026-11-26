// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { fileURLToPath } from 'node:url'
import { viewCatalog } from './catalog'

// For the generator script and its test only: this reads a path in the
// repository, so nothing the console bundles imports it.

/**
 * The playbook the assistant reads, with `temps_playbook`, before drawing a
 * result. It is generated from the catalog, so the components the model is
 * told about are the ones the console can draw; as a playbook it costs
 * tokens only in the turns that draw one.
 */
export const ASSISTANT_VIEWS_FILE = fileURLToPath(
  new URL(
    '../../../../../crates/temps-ai-chat/harness/show-results-as-views.md',
    import.meta.url
  )
)

/** When the assistant is to read it: the line its playbook index shows. */
const PLAYBOOK_HEADER = `---
name: show-results-as-views
description: Read before answering with a list of records, a few figures, a measure over time or one record's facts, to draw it as a view the console renders instead of a Markdown table
contexts: assistant
---

`

/** The file's contents, as the catalog generates them. */
export function assistantViewsInstructions(): string {
  return `${PLAYBOOK_HEADER}${viewCatalog.prompt().trim()}\n`
}
