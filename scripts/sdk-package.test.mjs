// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Exercise the built public entrypoint with Node, not TypeScript's bundler resolver.
import assert from 'node:assert/strict';
import { pathToFileURL } from 'node:url';
import { resolve } from 'node:path';
import { readFile } from 'node:fs/promises';

const root = resolve(process.argv[2]);
const manifest = JSON.parse(await readFile(resolve(root, 'package.json'), 'utf8'));
const entries = manifest.exports ?? { '.': manifest.main };
for (const [subpath, entry] of Object.entries(entries)) {
  const target = typeof entry === 'string' ? entry : entry.import;
  if (!target) continue;
  const loaded = await import(pathToFileURL(resolve(root, target)).href);
  assert.ok(Object.keys(loaded).length, `${manifest.name}${subpath} exports nothing`);
}
console.log(`${manifest.name}: public Node imports pass`);
