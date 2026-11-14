// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test';
import { resolve } from 'node:path';

test('npm CLI builds a Node executable with the bundled skill', async () => {
  const process = Bun.spawn(['bun', 'run', 'scripts/build.ts'], {
    cwd: resolve(import.meta.dir, '..'), stdout: 'ignore', stderr: 'inherit',
  });
  expect(await process.exited).toBe(0);
  expect(await Bun.file(resolve(import.meta.dir, '../dist/index.js')).text()).toStartWith('#!/usr/bin/env node\n');
  expect(await Bun.file(resolve(import.meta.dir, '../dist/skills/temps/SKILL.md')).exists()).toBe(true);
});
