// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { ToolCard, toolLabel, processToolSummary } from './DebugChatPanel'

const tool = { id: 'call-1', name: 'bash', arguments: '{"command":"npm test"}' }

test('running tool shows its type and input with a spinner, without raw panels', () => {
  const html = renderToStaticMarkup(<ToolCard tool={tool} />)
  expect(html).toContain('Bash · npm test')
  expect(html).toContain('Tool running')
  expect(html).toContain('animate-spin')
  expect(html).not.toContain('Arguments')
  expect(html).not.toContain('Result')
  expect(html).not.toContain('Waiting for next harness update')
})

test('completed tools stop spinning and keep output collapsed', () => {
  const html = renderToStaticMarkup(<ToolCard tool={{ ...tool, result: 'all tests passed' }} />)
  expect(html).toContain('Tool completed')
  expect(html).not.toContain('animate-spin')
  expect(html).not.toContain('all tests passed')
})

test('failed tools show the failure without requiring expansion', () => {
  const html = renderToStaticMarkup(<ToolCard tool={{ ...tool, result: 'Connection refused\nProcess exited with code 7.' }} />)
  expect(html).toContain('Failed')
  expect(html).toContain('Connection refused')
  expect(html).toContain('role="alert"')
  expect(html).not.toContain('animate-spin')
})

test('Claude exit receipts show nonzero failures without expanding', () => {
  const html = renderToStaticMarkup(<ToolCard tool={{ ...tool, result: 'Exit code 7\ncontrolled failure' }} />)
  expect(html).toContain('Failed')
  expect(html).toContain('controlled failure')
  expect(html).not.toContain('animate-spin')
  const success = renderToStaticMarkup(<ToolCard tool={{ ...tool, result: 'Exit code 0\nok' }} />)
  expect(success).toContain('Tool completed')
})

test('managed process start shows semantic input while loading, not MCP plumbing', () => {
  const html = renderToStaticMarkup(<ToolCard tool={{
    id: 'process-start',
    name: 'mcp__temps-chat__temps_process_start',
    arguments: JSON.stringify({ name: 'web', program: 'npm', args: ['run', 'dev'], directory: 'projects/app' }),
  }} />)
  expect(html).toContain('Start process · web · npm')
  expect(html).toContain('Tool running')
  expect(html).not.toContain('mcp__')
  expect(html).not.toContain('control.sock')
})

test('managed process operations identify their target and tolerate partial input', () => {
  for (const [operation, label] of [['status', 'Process status'], ['logs', 'Process logs'], ['stop', 'Stop process'], ['restart', 'Restart process']]) {
    expect(toolLabel({ id: 'process-call', name: `temps_process_${operation}`, arguments: '{"process_id":"process-1"}' }))
      .toBe(`${label} · process-1`)
    expect(toolLabel({ id: 'process-call', name: `temps_process_${operation}`, arguments: '{' }))
      .toBe(label)
    expect(toolLabel({ id: 'process-call', name: `temps_process_${operation}`, arguments: 'null' }))
      .toBe(label)
  }
})

test('managed process failure stays visible without expanding transport details', () => {
  const html = renderToStaticMarkup(<ToolCard tool={{
    id: 'process-call', name: 'temps_process_stop', arguments: '{"process_id":"process-1"}',
    result: JSON.stringify({ is_error: true, error: 'Process process-1 was not found in this workspace.' }),
  }} />)
  expect(html).toContain('Stop process · process-1')
  expect(html).toContain('Failed')
  expect(html).toContain('was not found in this workspace')
  expect(html).not.toContain('is_error')
  expect(html).not.toContain('animate-spin')
})

test('managed process receipt shows lifecycle without claiming HTTP readiness', () => {
  const html = renderToStaticMarkup(<ToolCard tool={{
    id: 'process-call', name: 'temps_process_start', arguments: '{"name":"web","program":"node"}',
    result: JSON.stringify({ type: 'process', process: { id: 'process-1', status: 'running', pid: 123 } }),
  }} />)
  expect(html).toContain('Running · PID 123')
  expect(html).toContain('Tool completed')
  expect(html).not.toContain('HTTP')
  expect(html).not.toContain('animate-spin')
})

test('process receipts distinguish exited state and bounded logs and reject unknown formats', () => {
  const processTool = { id: 'process-call', name: 'temps_process_status', arguments: '{}' }
  expect(processToolSummary({ ...processTool, result: '{"type":"process","process":{"status":"exited"}}' })).toBe('Exited')
  expect(processToolSummary({ ...processTool, result: '{"type":"process","process":{"status":"cancelled"}}' })).toBe('Stopped')
  expect(processToolSummary({ ...processTool, result: '{"type":"process","process":{"status":"succeeded"}}' })).toBe('Exited successfully')
  expect(processToolSummary({ ...processTool, name: 'temps_process_logs', result: '{"type":"logs","lines":[{}],"truncated":true}' })).toBe('1 log line · Limited output')
  expect(processToolSummary({ ...processTool, result: '{"type":"process","process":{"status":"failed","detail":"Executable not found"}}' })).toBe('Failed · Executable not found')
  for (const result of ['null', '{', '{"type":"process","process":{"status":"constructor"}}']) {
    expect(processToolSummary({ ...processTool, result })).toBeUndefined()
  }
  expect(processToolSummary({ ...processTool, name: 'bash', result: '{"type":"process","process":{"status":"running"}}' })).toBeUndefined()
})
