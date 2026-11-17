// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, it } from 'vitest';
import ts from 'typescript';
import { resolve } from 'node:path';

it('accepts supported upload options and rejects metadata the server cannot store', () => {
  const filename = resolve('src/__tests__/upload-options-typecheck.ts');
  const source = `
    import type { PutOptions } from '../types.js';
    const supported: PutOptions = { contentType: 'text/plain', addRandomSuffix: true };
    const cache: PutOptions = { cacheControl: 'max-age=60' };
    const disposition: PutOptions = { contentDisposition: 'inline' };
    const encoding: PutOptions = { contentEncoding: 'gzip' };
  `;
  const options: ts.CompilerOptions = { noEmit: true, strict: true, skipLibCheck: true };
  const host = ts.createCompilerHost(options);
  const getSourceFile = host.getSourceFile.bind(host);
  host.getSourceFile = (path, languageVersion, onError, shouldCreateNewSourceFile) =>
    path === filename
      ? ts.createSourceFile(path, source, languageVersion, true)
      : getSourceFile(path, languageVersion, onError, shouldCreateNewSourceFile);
  const program = ts.createProgram([filename], options, host);
  const errors = ts.getPreEmitDiagnostics(program);
  expect(errors.map(error => error.code)).toEqual([2353, 2353, 2353]);
  for (const option of ['cacheControl', 'contentDisposition', 'contentEncoding']) {
    expect(errors.some(error => ts.flattenDiagnosticMessageText(error.messageText, '\n').includes(option))).toBe(true);
  }
});
