import { test, expect, describe } from 'bun:test'
import {
  extractAiEvent,
  levelColor,
  parseCpuLimit,
  parseErrorGroupId,
  parseMemoryLimitMb,
  readApiError,
  statusColor,
  summarizeToolInput,
  truncate,
  validateRunArgs,
} from './index.js'

describe('statusColor', () => {
  test('flags terminal-success statuses without losing the original casing', () => {
    expect(statusColor('completed')).toContain('completed')
    expect(statusColor('COMPLETED')).toContain('COMPLETED')
  })

  test('treats failed, cancelled and no_fix all as failures', () => {
    expect(statusColor('failed')).toContain('failed')
    expect(statusColor('cancelled')).toContain('cancelled')
    expect(statusColor('no_fix')).toContain('no_fix')
  })

  test('flags in-flight statuses as running/pending', () => {
    expect(statusColor('running')).toContain('running')
    expect(statusColor('pending')).toContain('pending')
  })

  test('passes an unrecognised status through unchanged rather than assigning it the wrong color', () => {
    expect(statusColor('queued')).toBe('queued')
  })
})

describe('levelColor', () => {
  test('pads every level to a fixed width so log lines align', () => {
    expect(levelColor('warn')).toContain('warn ')
    expect(levelColor('info')).toContain('info ')
    expect(levelColor('error')).toContain('error')
    expect(levelColor('success')).toContain('success')
  })

  test('matches "warn" and "warning" the same way', () => {
    expect(levelColor('warn')).toContain('warn ')
    expect(levelColor('warning')).toContain('warning')
  })
})

describe('truncate', () => {
  test('collapses internal whitespace/newlines to single spaces', () => {
    expect(truncate('  hello \n  world  ')).toBe('hello world')
  })

  test('leaves short strings untouched', () => {
    expect(truncate('short', 40)).toBe('short')
  })

  test('truncates long strings and appends an ellipsis marker', () => {
    const long = 'x'.repeat(200)
    const out = truncate(long, 160)
    expect(out.length).toBe(161)
    expect(out.endsWith('…')).toBe(true)
  })
})

describe('summarizeToolInput', () => {
  test('returns empty string when there is no input yet (call still pending)', () => {
    expect(summarizeToolInput('Bash', undefined)).toBe('')
  })

  test('read/write/edit prefer file_path, falling back to path', () => {
    expect(summarizeToolInput('Read', { file_path: '/a.ts' })).toBe('/a.ts')
    expect(summarizeToolInput('write', { path: '/b.ts' })).toBe('/b.ts')
    expect(summarizeToolInput('Edit', { file_path: '/c.ts', path: '/d.ts' })).toBe('/c.ts')
  })

  test('bash/command are prefixed with "$" and truncated', () => {
    expect(summarizeToolInput('bash', { command: 'ls -la' })).toBe('$ ls -la')
    expect(summarizeToolInput('command', { command: 'pwd' })).toBe('$ pwd')
  })

  test('glob/grep surface the pattern', () => {
    expect(summarizeToolInput('Glob', { pattern: '**/*.ts' })).toBe('**/*.ts')
    expect(summarizeToolInput('Grep', { pattern: 'TODO' })).toBe('TODO')
  })

  test('webfetch/fetch surface the url, websearch/search the query', () => {
    expect(summarizeToolInput('WebFetch', { url: 'https://temps.sh' })).toBe('https://temps.sh')
    expect(summarizeToolInput('fetch', { url: 'https://x.com' })).toBe('https://x.com')
    expect(summarizeToolInput('WebSearch', { query: 'temps docs' })).toBe('temps docs')
    expect(summarizeToolInput('search', { query: 'foo' })).toBe('foo')
  })

  test('todowrite is summarised as "todos" rather than dumping the list', () => {
    expect(summarizeToolInput('TodoWrite', { todos: [{ content: 'a' }] })).toBe('todos')
  })

  test('task prefers prompt over description', () => {
    expect(summarizeToolInput('Task', { prompt: 'do the thing', description: 'short' })).toBe(
      'do the thing'
    )
    expect(summarizeToolInput('Task', { description: 'short' })).toBe('short')
  })

  test('unknown tools fall back to the first short string value so something is shown', () => {
    expect(summarizeToolInput('some_custom_tool', { foo: 'bar', count: 3 })).toBe('bar')
  })

  test('unknown tools with no usable string value render as empty rather than throwing', () => {
    expect(summarizeToolInput('some_custom_tool', { count: 3, nested: { a: 1 } })).toBe('')
  })
})

describe('extractAiEvent — Claude stream-json', () => {
  test('assistant tool_use becomes a call event', () => {
    const event = extractAiEvent(
      JSON.stringify({
        type: 'assistant',
        message: {
          content: [{ type: 'tool_use', id: 'toolu_1', name: 'Bash', input: { command: 'ls -la' } }],
        },
      })
    )
    expect(event).toEqual({ kind: 'call', toolUseId: 'toolu_1', name: 'Bash', summary: '$ ls -la' })
  })

  test('assistant thinking becomes a thinking event', () => {
    const event = extractAiEvent(
      JSON.stringify({
        type: 'assistant',
        message: { content: [{ type: 'thinking', thinking: 'Let me consider this.' }] },
      })
    )
    expect(event).toEqual({ kind: 'thinking', toolUseId: '', summary: 'Let me consider this.' })
  })

  test('assistant text becomes a text event', () => {
    const event = extractAiEvent(
      JSON.stringify({ type: 'assistant', message: { content: [{ type: 'text', text: 'Hello' }] } })
    )
    expect(event).toEqual({ kind: 'text', toolUseId: '', summary: 'Hello' })
  })

  test('an empty thinking block is skipped in favour of the next usable block', () => {
    const event = extractAiEvent(
      JSON.stringify({
        type: 'assistant',
        message: {
          content: [{ type: 'thinking', thinking: '   ' }, { type: 'text', text: 'Answer' }],
        },
      })
    )
    expect(event).toEqual({ kind: 'text', toolUseId: '', summary: 'Answer' })
  })

  test('user tool_result with string content becomes a result event', () => {
    const event = extractAiEvent(
      JSON.stringify({
        type: 'user',
        message: {
          content: [
            { type: 'tool_result', tool_use_id: 'toolu_1', content: 'ok', is_error: false },
          ],
        },
      })
    )
    expect(event).toEqual({ kind: 'result', toolUseId: 'toolu_1', summary: 'ok', isError: false })
  })

  test('user tool_result with array content joins and collapses whitespace', () => {
    const event = extractAiEvent(
      JSON.stringify({
        type: 'user',
        message: {
          content: [
            {
              type: 'tool_result',
              tool_use_id: 'toolu_2',
              content: [{ text: 'line1' }, { content: 'line2' }],
              is_error: true,
            },
          ],
        },
      })
    )
    expect(event).toEqual({
      kind: 'result',
      toolUseId: 'toolu_2',
      summary: 'line1 line2',
      isError: true,
    })
  })

  test('a block type it does not recognise yields null instead of throwing', () => {
    expect(
      extractAiEvent(
        JSON.stringify({ type: 'assistant', message: { content: [{ type: 'unknown' }] } })
      )
    ).toBeNull()
  })
})

describe('extractAiEvent — Codex item envelopes', () => {
  test('reasoning item becomes a thinking event', () => {
    const event = extractAiEvent(
      JSON.stringify({ type: 'item.started', item: { type: 'reasoning', text: 'planning' } })
    )
    expect(event).toEqual({ kind: 'thinking', toolUseId: '', summary: 'planning' })
  })

  test('command_execution start/end pair becomes call then result', () => {
    const start = extractAiEvent(
      JSON.stringify({
        type: 'item.started',
        item: { type: 'command_execution', id: 'cmd1', command: 'npm test' },
      })
    )
    expect(start).toEqual({ kind: 'call', toolUseId: 'cmd1', name: 'command', summary: '$ npm test' })

    const end = extractAiEvent(
      JSON.stringify({
        type: 'item.completed',
        item: { type: 'command_execution', id: 'cmd1', exit_code: 0, stdout: 'All good', stderr: '' },
      })
    )
    expect(end).toEqual({ kind: 'result', toolUseId: 'cmd1', summary: 'All good', isError: false })
  })

  test('a non-zero exit code with no stdout/stderr is still flagged as an error', () => {
    const event = extractAiEvent(
      JSON.stringify({
        type: 'item.completed',
        item: { type: 'command_execution', id: 'cmd2', status: 2 },
      })
    )
    expect(event).toEqual({ kind: 'result', toolUseId: 'cmd2', summary: 'exit 2', isError: true })
  })

  test('mcp_tool_call start/end pair', () => {
    const start = extractAiEvent(
      JSON.stringify({
        type: 'item.started',
        item: { type: 'mcp_tool_call', id: 'call1', server: 'srv', tool: 'do_thing' },
      })
    )
    expect(start).toEqual({ kind: 'call', toolUseId: 'call1', name: 'do_thing', summary: 'srv::do_thing' })

    const end = extractAiEvent(
      JSON.stringify({
        type: 'item.completed',
        item: { type: 'mcp_tool_call', id: 'call1', result: 'done', is_error: false },
      })
    )
    expect(end).toEqual({ kind: 'result', toolUseId: 'call1', summary: 'done', isError: false })
  })

  test('file_change start/end pair', () => {
    const start = extractAiEvent(
      JSON.stringify({
        type: 'item.started',
        item: { type: 'file_change', id: 'fc1', path: 'src/a.ts', operation: 'edit' },
      })
    )
    expect(start).toEqual({ kind: 'call', toolUseId: 'fc1', name: 'file', summary: 'edit src/a.ts' })

    const end = extractAiEvent(
      JSON.stringify({
        type: 'item.completed',
        item: { type: 'file_change', id: 'fc1', path: 'src/a.ts' },
      })
    )
    expect(end).toEqual({ kind: 'result', toolUseId: 'fc1', summary: 'ok src/a.ts' })
  })

  test('web_search start/end pair', () => {
    const start = extractAiEvent(
      JSON.stringify({
        type: 'item.started',
        item: { type: 'web_search', id: 'ws1', query: 'temps docs' },
      })
    )
    expect(start).toEqual({ kind: 'call', toolUseId: 'ws1', name: 'web_search', summary: 'Search temps docs' })

    const end = extractAiEvent(
      JSON.stringify({
        type: 'item.completed',
        item: { type: 'web_search', id: 'ws1', results: [1, 2, 3] },
      })
    )
    expect(end).toEqual({ kind: 'result', toolUseId: 'ws1', summary: '3 result(s)' })
  })

  test('an unrecognised item type yields null', () => {
    expect(
      extractAiEvent(JSON.stringify({ type: 'item.started', item: { type: 'mystery' } }))
    ).toBeNull()
  })
})

describe('extractAiEvent — OpenCode', () => {
  test('reasoning becomes a thinking event', () => {
    const event = extractAiEvent(JSON.stringify({ type: 'reasoning', part: { text: 'considering' } }))
    expect(event).toEqual({ kind: 'thinking', toolUseId: '', summary: 'considering' })
  })

  test('a running tool becomes a call event with the name capitalised', () => {
    const event = extractAiEvent(
      JSON.stringify({
        type: 'tool',
        part: { name: 'bash', id: 'call1', state: 'running', input: { command: 'ls' } },
      })
    )
    expect(event).toEqual({ kind: 'call', toolUseId: 'call1', name: 'Bash', summary: '$ ls' })
  })

  test('falls back to parsing a JSON-string args field when input is absent', () => {
    const event = extractAiEvent(
      JSON.stringify({
        type: 'tool',
        part: { name: 'grep', id: 'call2', state: 'pending', args: JSON.stringify({ pattern: 'foo' }) },
      })
    )
    expect(event).toEqual({ kind: 'call', toolUseId: 'call2', name: 'Grep', summary: 'foo' })
  })

  test('a completed tool becomes a result event', () => {
    const event = extractAiEvent(
      JSON.stringify({
        type: 'tool',
        part: { name: 'bash', id: 'call1', state: 'completed', result: 'output here' },
      })
    )
    expect(event).toEqual({ kind: 'result', toolUseId: 'call1', summary: 'output here', isError: false })
  })

  test('an errored tool with no output still reports isError rather than looking empty', () => {
    const event = extractAiEvent(
      JSON.stringify({ type: 'tool', part: { name: 'bash', id: 'call1', state: 'error', output: '' } })
    )
    expect(event).toEqual({ kind: 'result', toolUseId: 'call1', summary: '(error)', isError: true })
  })

  test('a tool in an unrecognised state yields null instead of a bogus call/result', () => {
    expect(
      extractAiEvent(JSON.stringify({ type: 'tool', part: { name: 'bash', id: 'call1', state: 'queued' } }))
    ).toBeNull()
  })

  test('text becomes a text event', () => {
    const event = extractAiEvent(JSON.stringify({ type: 'text', part: { text: 'final answer' } }))
    expect(event).toEqual({ kind: 'text', toolUseId: '', summary: 'final answer' })
  })

  test('step_start is a silent turn boundary', () => {
    expect(extractAiEvent(JSON.stringify({ type: 'step_start' }))).toEqual({
      kind: 'step',
      toolUseId: '',
      summary: '',
    })
  })

  test('step_finish is suppressed for a plain tool-use turn', () => {
    expect(
      extractAiEvent(JSON.stringify({ type: 'step_finish', part: { reason: 'tool-calls' } }))
    ).toBeNull()
  })

  test('step_finish with a real stop reason reports the token counts', () => {
    const event = extractAiEvent(
      JSON.stringify({
        type: 'step_finish',
        part: { reason: 'stop', tokens: { input: 120, output: 45 } },
      })
    )
    expect(event).toEqual({
      kind: 'step_end',
      toolUseId: '',
      summary: 'stop · 120 in / 45 out',
    })
  })

  test('message.part.updated delegates to the tool-event branch', () => {
    const event = extractAiEvent(
      JSON.stringify({
        type: 'message.part.updated',
        part: { type: 'tool', name: 'bash', id: 'call9', state: 'running', input: { command: 'pwd' } },
      })
    )
    expect(event).toEqual({ kind: 'call', toolUseId: 'call9', name: 'Bash', summary: '$ pwd' })
  })

  test('message.part.updated for a non-tool part yields null', () => {
    expect(
      extractAiEvent(JSON.stringify({ type: 'message.part.updated', part: { type: 'other' } }))
    ).toBeNull()
  })
})

describe('extractAiEvent — malformed input', () => {
  test('invalid JSON returns null rather than throwing', () => {
    expect(extractAiEvent('not json{{{')).toBeNull()
  })

  test('an unrecognised top-level type returns null', () => {
    expect(extractAiEvent(JSON.stringify({ type: 'something_else' }))).toBeNull()
  })
})

describe('validateRunArgs', () => {
  test('rejects passing both a slug and --from-file', () => {
    expect(validateRunArgs('my-workflow', { fromFile: 'run.yaml' })).toBe(
      'Pass either <slug> or --from-file, not both.'
    )
  })

  test('rejects passing neither', () => {
    expect(validateRunArgs(undefined, {})).toBe(
      'Specify a workflow slug, or use --from-file <path> for an ephemeral run.'
    )
  })

  test('rejects --cpu/--memory without --from-file', () => {
    expect(validateRunArgs('my-workflow', { cpu: '1' })).toBe(
      '--cpu / --memory only apply to ephemeral runs (use --from-file).'
    )
    expect(validateRunArgs('my-workflow', { memory: '512' })).toBe(
      '--cpu / --memory only apply to ephemeral runs (use --from-file).'
    )
  })

  test('accepts a bare slug', () => {
    expect(validateRunArgs('my-workflow', {})).toBeNull()
  })

  test('accepts --from-file with --cpu/--memory', () => {
    expect(validateRunArgs(undefined, { fromFile: 'run.yaml', cpu: '1', memory: '512' })).toBeNull()
  })
})

describe('parseErrorGroupId', () => {
  test('passes through undefined/empty as "not provided"', () => {
    expect(parseErrorGroupId(undefined)).toBeUndefined()
  })

  test('accepts a positive integer string', () => {
    expect(parseErrorGroupId('42')).toBe(42)
  })

  test('rejects zero, negatives and non-integers with the flag name in the message', () => {
    expect(() => parseErrorGroupId('0')).toThrow(/--error-group/)
    expect(() => parseErrorGroupId('-1')).toThrow(/positive integer/)
    expect(() => parseErrorGroupId('1.5')).toThrow(/positive integer/)
    expect(() => parseErrorGroupId('abc')).toThrow(/positive integer/)
  })
})

describe('parseCpuLimit', () => {
  test('passes through when not provided', () => {
    expect(parseCpuLimit(undefined)).toBeUndefined()
  })

  test('accepts a fractional core count', () => {
    expect(parseCpuLimit('0.5')).toBe(0.5)
  })

  test('rejects zero, negative and non-numeric values', () => {
    expect(() => parseCpuLimit('0')).toThrow(/Invalid --cpu value/)
    expect(() => parseCpuLimit('-2')).toThrow(/Invalid --cpu value/)
    expect(() => parseCpuLimit('nope')).toThrow(/Invalid --cpu value/)
  })
})

describe('parseMemoryLimitMb', () => {
  test('passes through when not provided', () => {
    expect(parseMemoryLimitMb(undefined)).toBeUndefined()
  })

  test('accepts a positive integer', () => {
    expect(parseMemoryLimitMb('512')).toBe(512)
  })

  test('rejects fractional, zero, negative and non-numeric values', () => {
    expect(() => parseMemoryLimitMb('512.5')).toThrow(/positive integer MB/)
    expect(() => parseMemoryLimitMb('0')).toThrow(/positive integer MB/)
    expect(() => parseMemoryLimitMb('-128')).toThrow(/positive integer MB/)
    expect(() => parseMemoryLimitMb('abc')).toThrow(/positive integer MB/)
  })
})

describe('readApiError', () => {
  test('prefers a problem-detail JSON body, combining title and detail', async () => {
    const response = new Response(JSON.stringify({ title: 'Not Found', detail: 'no such run' }), {
      status: 404,
      statusText: 'Not Found',
    })
    const err = await readApiError(response)
    expect(err.message).toBe('Not Found — no such run')
  })

  test('uses just the title when there is no detail', async () => {
    const response = new Response(JSON.stringify({ title: 'Bad Request' }), { status: 400 })
    const err = await readApiError(response)
    expect(err.message).toBe('Bad Request')
  })

  test('falls back to raw body text when it is not JSON', async () => {
    const response = new Response('boom', { status: 500, statusText: 'Internal Server Error' })
    const err = await readApiError(response)
    expect(err.message).toBe('HTTP 500: boom')
  })

  test('falls back to the status text when the body is empty', async () => {
    const response = new Response('', { status: 503, statusText: 'Service Unavailable' })
    const err = await readApiError(response)
    expect(err.message).toBe('HTTP 503: Service Unavailable')
  })
})
