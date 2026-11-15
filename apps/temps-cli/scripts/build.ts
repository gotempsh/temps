// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { chmod, cp, mkdir, rm } from 'node:fs/promises'
import { basename } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = fileURLToPath(new URL('../', import.meta.url))
const dist = root + 'dist'
await rm(dist, { recursive: true, force: true })
const result = await Bun.build({
  entrypoints: [root + 'src/index.ts'],
  outdir: dist,
  target: 'node',
})
if (!result.success) throw new AggregateError(result.logs, 'CLI build failed')
const entry = dist + '/index.js'
await Bun.write(entry, (await Bun.file(entry).text()).replace(/^#![^\n]*/, '#!/usr/bin/env node'))
await chmod(entry, 0o755)
await mkdir(dist + '/skills', { recursive: true })
await cp(root + '../../skills/temps', dist + '/skills/temps', {
  recursive: true,
  filter: (source) => basename(source) !== '__pycache__' && !source.endsWith('.pyc'),
})
