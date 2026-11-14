// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { Script } from 'node:vm';

for (const name of ['temps.js', 'temps.min.js']) {
  const source = await readFile(new URL(`../dist/${name}`, import.meta.url), 'utf8');
  assert.ok(source.length > 0, `${name} is empty`);
  // A CDN script must parse as a classic script, without a module loader or
  // Node globals, and defer initialization while the document is loading.
  let listener;
  new Script(source, { filename: name }).runInNewContext({
    document: {
      readyState: 'loading',
      addEventListener(event, callback, options) {
        assert.equal(event, 'DOMContentLoaded');
        assert.equal(options.once, true);
        listener = callback;
      },
    },
    setTimeout, clearTimeout, setInterval, clearInterval, console,
  }, { timeout: 1000 });
  assert.equal(typeof listener, 'function', `${name} did not register auto-init`);
}
console.log('Both CDN bundles load as standalone classic scripts');
