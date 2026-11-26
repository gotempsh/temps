// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Writes what the assistant is told about views to the file the server
// compiles into its instructions. Run it after changing the view catalog or
// its prompt: `bun run scripts/write-assistant-views.ts`.

import { writeFileSync } from 'node:fs'
import {
  ASSISTANT_VIEWS_FILE,
  assistantViewsInstructions,
} from '../src/components/ai/views/instructions'

writeFileSync(ASSISTANT_VIEWS_FILE, assistantViewsInstructions())
process.stdout.write('Wrote the assistant view instructions.\n')
