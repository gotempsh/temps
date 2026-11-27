// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import {
  isTempsAgentTranscript,
  parseTempsAgentTranscript,
  resultLine,
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

test('a transcript whose earlier steps were left out says so, before what it kept', () => {
  const output = [
    JSON.stringify({ type: 'temps.truncated', dropped_lines: 7 }),
    JSON.stringify({ type: 'temps.text', text: 'Done.' }),
    JSON.stringify({ type: 'temps.finish', reason: 'stop', turns: 9 }),
  ].join('\n')
  expect(isTempsAgentTranscript(output)).toBe(true)
  const parsed = parseTempsAgentTranscript(output)
  const notice =
    "7 earlier steps are not shown here: the run was long. The run's log has every step."
  expect(parsed[0]).toEqual({ type: 'notice', content: notice })
  expect(parsed[1]).toEqual({ type: 'assistant_text', content: 'Done.' })
  // The conversation view shows the notice itself, not a bare "Result".
  expect(resultLine(parsed[0]!)).toEqual({
    label: 'Not shown',
    summary: notice,
  })
  expect(resultLine(parsed[2]!)).toEqual({
    label: 'Result',
    summary: '9 turns',
  })
})
