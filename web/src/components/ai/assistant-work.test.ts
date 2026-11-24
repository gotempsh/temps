// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import {
  failedToolCount,
  standaloneResultId,
  groupAssistantParts,
  liveWorkSegmentKey,
  liveWorkStatus,
  runningTool,
  toolActivity,
  traceStep,
  workSummary,
  type AssistantSegment,
} from './assistant-work'
import type { ChatPart, ToolCall } from './chat-message-parts'

const tool = (
  id: string,
  name: string,
  args: Record<string, unknown> = {},
  result = '{"ok":true}'
): ToolCall => ({ id, name, arguments: JSON.stringify(args), result })

/** A tool still running: no result yet. */
const running = (
  id: string,
  name: string,
  args: Record<string, unknown> = {}
): ToolCall => ({ id, name, arguments: JSON.stringify(args) })

const text = (value: string): ChatPart => ({ type: 'text', text: value })
const call = (value: ToolCall): ChatPart => ({ type: 'tool', tool: value })

const everyTool = () => true

function shape(segments: AssistantSegment[]): string[] {
  return segments.map((segment) =>
    segment.kind === 'work'
      ? `work[${segment.steps.map((step) => step.tool.id).join(',')}]`
      : segment.part.type === 'text'
        ? `text:${segment.part.text}`
        : segment.part.type === 'tool'
          ? `card:${segment.part.tool.id}`
          : 'permission'
  )
}

describe('groupAssistantParts', () => {
  it('folds only the tools, keeping every word the model wrote in place', () => {
    const parts = [
      text('Let me look.'),
      call(tool('a', 'sandbox_exec', { command: 'ls' })),
      call(tool('b', 'sandbox_exec', { command: 'pwd' })),
      text('Now the config.'),
      call(tool('c', 'sandbox_read_file', { path: 'app.json' })),
      text('It is set to port 3000.'),
    ]
    expect(shape(groupAssistantParts(parts, everyTool))).toEqual([
      'text:Let me look.',
      'work[a,b]',
      'text:Now the config.',
      'work[c]',
      'text:It is set to port 3000.',
    ])
  })

  it('leaves an answer with no tools exactly as it is', () => {
    const parts = [text('Hello'), text('there')]
    expect(shape(groupAssistantParts(parts, everyTool))).toEqual([
      'text:Hello',
      'text:there',
    ])
  })

  it('keeps a card that needs the user out of the fold, with its lead-in', () => {
    const parts = [
      call(tool('a', 'sandbox_exec', { command: 'ls' })),
      text('I need to create a project. Approve?'),
      call(tool('w', 'temps_write')),
      call(tool('b', 'sandbox_exec', { command: 'pwd' })),
      text('Done.'),
    ]
    const segments = groupAssistantParts(
      parts,
      (value) => value.name !== 'temps_write'
    )
    expect(shape(segments)).toEqual([
      'work[a]',
      'text:I need to create a project. Approve?',
      'card:w',
      'work[b]',
      'text:Done.',
    ])
  })

  it('keys a group by where it starts, so it keeps its state as it grows', () => {
    const before = groupAssistantParts(
      [text('x'), call(tool('a', 'Bash', { command: 'ls' }))],
      everyTool
    )
    const after = groupAssistantParts(
      [
        text('x'),
        call(tool('a', 'Bash', { command: 'ls' })),
        call(tool('b', 'Bash', { command: 'pwd' })),
      ],
      everyTool
    )
    expect(before[1]).toMatchObject({ kind: 'work', key: 'work-1' })
    expect(after[1]).toMatchObject({ kind: 'work', key: 'work-1' })
  })

  it('does not split a group on blank text', () => {
    const parts = [
      call(tool('a', 'Bash', { command: 'ls' })),
      text('  \n'),
      call(tool('b', 'Bash', { command: 'pwd' })),
    ]
    expect(shape(groupAssistantParts(parts, everyTool))).toEqual(['work[a,b]'])
  })
})

describe('standaloneResultId', () => {
  const isLookup = (value: ToolCall) => value.name === 'temps'
  const hasOwnView = (value: ToolCall) => value.id.startsWith('list')
  const standalone = (parts: ChatPart[]) =>
    standaloneResultId(parts, isLookup, hasOwnView)

  it('is the last viewable result of an answer that only looked things up', () => {
    expect(
      standalone([
        call(tool('help', 'temps')),
        call(tool('list-page-1', 'temps')),
        text('Fetching the rest.'),
        call(tool('list-all', 'temps')),
        call(tool('environments', 'temps')),
        text('You have 15 projects.'),
      ])
    ).toBe('list-all')
  })

  it('is none once the answer does other work, wherever the lookup sits', () => {
    expect(
      standalone([
        call(tool('playbook', 'temps_playbook')),
        call(tool('list-projects', 'temps')),
      ])
    ).toBeNull()
    expect(
      standalone([
        call(tool('list-projects', 'temps')),
        call(tool('exec', 'sandbox_exec')),
      ])
    ).toBeNull()
  })

  it('is none when no lookup has a view of its own', () => {
    expect(standalone([call(tool('help', 'temps'))])).toBeNull()
    expect(standalone([text('Hello')])).toBeNull()
  })
})

describe('liveWorkSegmentKey', () => {
  const work = [
    call(tool('a', 'sandbox_exec', { command: 'ls' })),
    text('The preview is up. Now the logs.'),
  ]

  it('is the last group while the turn runs it', () => {
    const segments = groupAssistantParts(work.slice(0, 1), everyTool)
    expect(liveWorkSegmentKey(segments, true)).toBe('work-0')
    expect(liveWorkSegmentKey(segments, false)).toBeNull()
  })

  it('ends once the model writes after it', () => {
    const segments = groupAssistantParts(work, everyTool)
    expect(shape(segments)).toEqual([
      'work[a]',
      'text:The preview is up. Now the logs.',
    ])
    expect(liveWorkSegmentKey(segments, true)).toBeNull()
  })

  it('has no live group when a card the user must answer follows it', () => {
    const segments = groupAssistantParts(
      [...work, call(tool('w', 'temps_write'))],
      (candidate) => candidate.name !== 'temps_write'
    )
    expect(liveWorkSegmentKey(segments, true)).toBeNull()
  })
})

describe('workSummary', () => {
  it('counts each kind of work, files once each', () => {
    const summary = workSummary([
      tool('1', 'sandbox_exec', { command: 'ls' }),
      tool('2', 'sandbox_exec', { command: 'pwd' }),
      tool('3', 'sandbox_read_file', { path: 'a.ts' }),
      tool('4', 'sandbox_read_file', { path: 'a.ts' }),
      tool('5', 'sandbox_read_file', { path: 'b.ts' }),
      tool('6', 'Edit', { file_path: 'a.ts' }),
    ])
    expect(summary).toBe('Ran 2 commands, read 2 files, edited a file')
  })

  it('names namespaced tools by their own name', () => {
    expect(
      workSummary([tool('1', 'mcp__sandbox__Bash', { command: 'x' })])
    ).toBe('Ran a command')
  })

  it('counts platform lookups and anything else', () => {
    expect(
      workSummary([
        tool('1', 'temps', { path: '/projects' }),
        tool('2', 'temps', { path: '/projects/1' }),
        tool('3', 'temps_goal'),
      ])
    ).toBe('Checked Temps 2 times, set a goal')
    expect(
      workSummary([tool('1', 'temps_goal'), tool('2', 'something_new')])
    ).toBe('Set a goal, used a tool')
    expect(
      workSummary([
        tool('1', 'temps', { path: '/projects' }),
        tool('2', 'temps', { path: '/projects/1' }),
        tool('3', 'temps_goal'),
      ])
    ).toBe('Checked Temps 2 times, set a goal')
  })
})

describe('failedToolCount', () => {
  it('counts only explicit failures', () => {
    expect(
      failedToolCount([
        tool('1', 'Bash', {}, '{"exit_code":1}'),
        tool('2', 'Bash', {}, '{"exit_code":0}'),
        running('3', 'Bash'),
      ])
    ).toBe(1)
  })
})

describe('toolActivity', () => {
  it('says what a step is doing', () => {
    expect(
      toolActivity(tool('1', 'sandbox_exec', { command: 'npm test\nmore' }))
    ).toBe('Running npm test')
    expect(toolActivity(tool('2', 'Read', { file_path: 'src/a.ts' }))).toBe(
      'Reading src/a.ts'
    )
    expect(
      toolActivity(tool('3', 'sandbox_write_file', { path: 'b.ts' }))
    ).toBe('Editing b.ts')
  })

  it('still says something while the arguments are streaming in', () => {
    const streaming: ToolCall = {
      id: '1',
      name: 'Bash',
      arguments: '{"comm',
      result: undefined,
    }
    expect(toolActivity(streaming)).toBe('Running a command')
  })
})

describe('runningTool', () => {
  it('follows the step still running', () => {
    const sleeping = running('b', 'Bash', { command: 'sleep 5' })
    expect(
      runningTool([
        { tool: tool('a', 'Bash'), index: 0 },
        { tool: sleeping, index: 1 },
      ])
    ).toBe(sleeping)
    expect(runningTool([{ tool: tool('a', 'Bash'), index: 0 }])).toBeNull()
  })
})

describe('liveWorkStatus', () => {
  it('names the step running now', () => {
    expect(
      liveWorkStatus([
        { tool: tool('a', 'Read', { file_path: 'a.ts' }), index: 0 },
        { tool: running('b', 'Bash', { command: 'npm test' }), index: 1 },
      ])
    ).toBe('Running npm test')
  })

  it('names an unrecognised running tool in plain words', () => {
    expect(
      liveWorkStatus([{ tool: running('a', 'temps_deploy_preview'), index: 0 }])
    ).toBe('Using Deploy preview')
  })

  it('between steps, names the last step taken and how far it has got', () => {
    const done = { tool: tool('a', 'Bash', { command: 'npm test' }), index: 0 }
    expect(liveWorkStatus([done])).toBe('Ran npm test · 1 step so far')
    expect(
      liveWorkStatus([
        done,
        { tool: tool('b', 'temps', { command: 'projects list' }), index: 1 },
      ])
    ).toBe('Ran projects list · 2 steps so far')
  })

  it('says when the last step failed', () => {
    const failed = tool('a', 'Read', { file_path: 'a.ts' }, '{"is_error":true}')
    expect(liveWorkStatus([{ tool: failed, index: 0 }])).toMatch(
      /^Failed: Read a\.ts · 1 step so far$/
    )
  })

  it('names the Temps operation running now', () => {
    expect(
      liveWorkStatus([
        {
          tool: running('a', 'temps', { command: 'projects get 49' }),
          index: 0,
        },
      ])
    ).toBe('Running projects get 49')
  })

  it('says nothing before the first step', () => {
    expect(liveWorkStatus([])).toBeNull()
  })
})

describe('traceStep', () => {
  it('names each step in plain words, never its raw arguments', () => {
    expect(
      traceStep(
        tool('1', 'temps_goal', {
          action: 'track',
          title: 'Deployment 43 goes live',
          operation_id: 'get_deployment',
        })
      )
    ).toEqual({ verb: 'Watch', detail: 'Deployment 43 goes live' })
    expect(
      traceStep(
        tool('2', 'temps', {
          command: 'deployments get_deployment --deployment_id 43',
        })
      )
    ).toEqual({
      verb: 'Temps',
      detail: 'deployments get_deployment --deployment_id 43',
    })
    expect(
      traceStep(tool('3', 'sandbox_exec', { command: 'ls\npwd' }))
    ).toEqual({ verb: 'Run', detail: 'ls' })
    expect(traceStep(tool('4', 'some_new_tool', { x: { y: 1 } }))).toEqual({
      verb: 'Some new tool',
      detail: null,
    })
  })

  it('counts the lines a step wrote or replaced', () => {
    expect(
      traceStep(
        tool('1', 'sandbox_write_file', { path: 'a.ts', content: 'a\nb\nc\n' })
      )
    ).toEqual({ verb: 'Write', detail: 'a.ts', added: 3 })
    expect(
      traceStep(
        tool('2', 'Edit', {
          file_path: 'b.ts',
          old_string: 'x',
          new_string: 'y\nz',
        })
      )
    ).toEqual({ verb: 'Edit', detail: 'b.ts', added: 2, removed: 1 })
  })
})
