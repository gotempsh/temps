// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { GlobalLogLine } from '@/api/client/types.gen'
import { groupLogLines, logVolume } from './log-explorer'
const line: GlobalLogLine = {
  timestamp: '2026-09-09T12:00:00Z',
  level: 'ERROR',
  message: 'Request failed',
  owner: 'Storefront',
  env: 'production',
  service: 'web',
  project_id: 1,
  chunk_id: 'one',
  line_offset: 0,
}
describe('loaded log aggregates', () => {
  test('counts severity buckets across minute boundaries and ignores invalid timestamps', () => {
    const result = logVolume([
      line,
      { ...line, timestamp: '2026-09-09T12:01:00Z', level: 'INFO' },
      { ...line, timestamp: 'invalid' },
    ])
    expect(result.step).toBe(60000)
    expect(result.buckets).toHaveLength(2)
    expect(result.buckets[0].ERROR).toBe(1)
    expect(result.buckets[1].INFO).toBe(1)
    expect(logVolume([]).buckets).toEqual([])
    expect(logVolume([{ ...line, timestamp: 'invalid' }]).buckets).toEqual([])
  })
  test('groups exact messages without treating distinct requests as the same pattern', () => {
    const groups = groupLogLines(
      [line, line, { ...line, message: 'Request 2 failed' }],
      'message'
    )
    expect(groups.map((g) => g.count)).toEqual([2, 1])
    expect(groups[0].errors).toBe(2)
  })
  test('keeps similarly named services from different projects separate', () => {
    expect(
      groupLogLines([line, { ...line, project_id: 2 }], 'service')
    ).toHaveLength(2)
    expect(groupLogLines([], 'service')).toEqual([])
  })
})
