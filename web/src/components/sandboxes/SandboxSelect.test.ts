// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import type { SandboxInner } from '@/api/client/types.gen'
import { pickableSandboxes, sandboxOptionLabel } from './SandboxSelect'

function sandbox(id: string, name: string, status: string): SandboxInner {
  return { id, name, status } as SandboxInner
}

test('a sandbox is offered by the name its owner gave it and its id', () => {
  expect(
    sandboxOptionLabel(sandbox('sbx_alpha', 'agent scratchpad', 'running'))
  ).toBe('agent scratchpad · sbx_alpha')
  expect(
    sandboxOptionLabel(sandbox('sbx_alpha', 'agent scratchpad', 'stopped'))
  ).toBe('agent scratchpad · sbx_alpha · stopped')
  expect(sandboxOptionLabel(sandbox('sbx_alpha', '', 'running'))).toBe(
    'sbx_alpha'
  )
})

test('destroyed sandboxes are not offered', () => {
  const listed = [
    sandbox('sbx_alpha', 'a', 'running'),
    sandbox('sbx_beta', 'b', 'destroyed'),
    sandbox('sbx_gamma', 'c', 'stopped'),
  ]
  expect(pickableSandboxes(listed).map((s) => s.id)).toEqual([
    'sbx_alpha',
    'sbx_gamma',
  ])
})
