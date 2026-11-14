// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { chmod, cp, mkdir, rm } from 'node:fs/promises';
import { resolve } from 'node:path';

// The npm artifact must run on Node on every supported build host.
const root = resolve(import.meta.dir, '..');
await rm(resolve(root, 'dist'), { recursive: true, force: true });
const result = await Bun.build({
  entrypoints: [resolve(root, 'src/index.ts')],
  outdir: resolve(root, 'dist'),
  target: 'node',
});
if (!result.success) throw new AggregateError(result.logs, 'CLI build failed');
const entry = Bun.file(resolve(root, 'dist/index.js'));
await Bun.write(entry, (await entry.text()).replace(/^#![^\n]*/, '#!/usr/bin/env node'));
await chmod(entry.name!, 0o755);
await mkdir(resolve(root, 'dist/skills'), { recursive: true });
await cp(resolve(root, '../../skills/temps'), resolve(root, 'dist/skills/temps'), { recursive: true });
