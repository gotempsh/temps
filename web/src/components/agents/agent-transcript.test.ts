// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import {
  isTempsAgentTranscript,
  parseTempsAgentTranscript,
} from './agent-transcript'

const transcript = [
  '{"type":"temps.tool_call","id":"c1","name":"sandbox_exec","input":{"command":"npm test"}}',
  '{"type":"temps.tool_result","id":"c1","output":"exit code 1","is_error":false}',
  '{"type":"temps.tool_call","id":"c2","name":"deploy","input":{}}',
  '{"type":"temps.tool_result","id":"c2","output":"There is no tool named `deploy`","is_error":true}',
  '{"type":"temps.text","text":"Fixed the null check."}',
  '{"type":"temps.finish","reason":"stop","turns":3}',
].join('\n')

test('a Temps agent transcript is told apart from a CLI stream', () => {
  expect(isTempsAgentTranscript(transcript)).toBe(true)
  expect(
    isTempsAgentTranscript('{"type":"assistant","message":{"content":[]}}')
  ).toBe(false)
  expect(isTempsAgentTranscript('plain text')).toBe(false)
})

test('each tool call carries its result, and a failed one says so', () => {
  expect(parseTempsAgentTranscript(transcript)).toEqual([
    {
      type: 'tool_call',
      tool: 'sandbox_exec',
      toolUseId: 'c1',
      toolInput: { command: 'npm test' },
      toolResult: 'exit code 1',
      toolStatus: 'completed',
    },
    {
      type: 'tool_call',
      tool: 'deploy',
      toolUseId: 'c2',
      toolInput: {},
      toolResult: 'There is no tool named `deploy`',
      toolStatus: 'failed',
    },
    { type: 'assistant_text', content: 'Fixed the null check.' },
    { type: 'result', result: 'The agent finished.', numTurns: 3 },
  ])
})

test('a live run shows a tool still running, and a capped run says it ran out of turns', () => {
  const live = parseTempsAgentTranscript(
    '{"type":"temps.tool_call","id":"c1","name":"sandbox_exec","input":{"command":"ls"}}'
  )
  expect(live[0].toolStatus).toBe('in_progress')
  expect(live[0].toolResult).toBeUndefined()
  expect(
    parseTempsAgentTranscript(
      '{"type":"temps.finish","reason":"max_turns","turns":30}'
    )
  ).toEqual([
    {
      type: 'result',
      result: 'The agent used all its turns before finishing.',
      numTurns: 30,
    },
  ])
})
