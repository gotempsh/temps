// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import type { ChatMessage, ToolCall } from './chat-message-parts'
import { AssistantBody } from './DebugChatPanel-components'

const noop = () => {}

function body(message: ChatMessage, streaming = false) {
  return renderToStaticMarkup(
    <QueryClientProvider client={new QueryClient()}>
      <AssistantBody
        message={message}
        streaming={streaming}
        activityLabel="Thinking"
        conversationPublicId="chat_1"
        userScoped
        onFix={noop}
        onPermissionResolved={noop}
      />
    </QueryClientProvider>
  )
}

const command = (id: string, cmd: string, result?: string): ToolCall => ({
  id,
  name: 'sandbox_exec',
  arguments: JSON.stringify({ command: cmd }),
  result,
})

const settled: ChatMessage = {
  role: 'assistant',
  content: '',
  parts: [
    { type: 'text', text: 'Checking the workspace first.' },
    { type: 'tool', tool: command('a', 'ls -la', '{"exit_code":0}') },
    {
      type: 'tool',
      tool: {
        id: 'b',
        name: 'sandbox_read_file',
        arguments: JSON.stringify({ path: 'package.json' }),
        result: '{"content":"{}"}',
      },
    },
    { type: 'tool', tool: command('c', 'npm test', '{"exit_code":1}') },
    { type: 'text', text: 'The tests fail on a missing module.' },
  ],
}

test('finished work folds into one summary line, and the answer stays in full', () => {
  const markup = body(settled)
  expect(markup).toContain('Ran 2 commands, read a file')
  expect(markup).toContain('1 failed')
  expect(markup).toContain('The tests fail on a missing module.')
  // The steps are behind the summary; what the model said stays in view.
  expect(markup).toContain('Checking the workspace first.')
  expect(markup).not.toContain('npm test')
  expect(markup).toContain('aria-expanded="false"')
})

test('text between tools stays in the conversation, outside the trace', () => {
  const markup = body(
    {
      role: 'assistant',
      content: '',
      parts: [
        { type: 'text', text: 'I will look at the fonts first.' },
        { type: 'tool', tool: command('a', 'ls', '{"exit_code":0}') },
        { type: 'text', text: 'The preview is up. Now the logs.' },
      ],
    },
    true
  )
  // The finished group closes to its summary; the words around it stay put,
  // and the one activity indicator follows the text being written.
  expect(markup).toContain('aria-expanded="false"')
  expect(markup).toContain('I will look at the fonts first.')
  expect(markup).toContain('The preview is up. Now the logs.')
  expect(markup.match(/role="status"/g)).toHaveLength(1)
  expect(markup.indexOf('I will look at the fonts first.')).toBeLessThan(
    markup.indexOf('aria-expanded')
  )
})

test('while the turn works, the trace stays closed on the step it is taking', () => {
  const markup = body(
    {
      role: 'assistant',
      content: '',
      parts: [
        { type: 'tool', tool: command('a', 'ls', '{"exit_code":0}') },
        { type: 'tool', tool: command('b', 'npm install') },
      ],
    },
    true
  )
  expect(markup).toContain('Thinking')
  expect(markup).toContain('aria-expanded="false"')
  // One line of state, not a row per tool.
  expect(markup).toContain('>Running npm install</p>')
  expect(markup).not.toContain('aria-label="Running"')
  expect(markup).not.toContain('Ran 2 commands')
  // The group's header is the activity: no second indicator after it.
  expect(markup.match(/role="status"/g)?.length).toBe(1)
})

test('between steps the working state names the last step and the count', () => {
  const parts = Array.from({ length: 7 }, (_, at) => ({
    type: 'tool' as const,
    tool: command(`t${at}`, `step-${at}`, '{"exit_code":0}'),
  }))
  const markup = body(
    {
      role: 'assistant',
      content: '',
      parts: [...parts, { type: 'text', text: '' }],
    },
    true
  )
  expect(markup).toContain('Ran step-6 · 7 steps so far')
  // Only the latest step, never the whole list on one line.
  expect(markup).not.toContain('step-5')
})

test('a settled goal step folds to what it did, not its raw arguments', () => {
  const markup = renderToStaticMarkup(
    <QueryClientProvider client={new QueryClient()}>
      <AssistantBody
        message={{
          role: 'assistant',
          content: 'Watching it.',
          parts: [
            {
              type: 'tool',
              tool: {
                id: 'g',
                name: 'temps_goal',
                arguments: JSON.stringify({
                  action: 'track',
                  title: 'Deployment 43 goes live',
                  operation_id: 'get_deployment',
                }),
                result: '{"ok":true}',
              },
            },
          ],
        }}
        streaming={false}
        activityLabel="Thinking"
        conversationPublicId="chat_1"
        userScoped
        onFix={noop}
        onPermissionResolved={noop}
      />
    </QueryClientProvider>
  )
  // A lone step is its own row: what it did, not a summary over it.
  expect(markup).not.toContain('Set a goal')
  expect(markup).toContain('>Watch<')
  expect(markup).toContain('>Deployment 43 goes live<')
  // The raw arguments stay in the hover title, never on screen.
  expect(markup.replace(/ title="[^"]*"/g, '')).not.toContain('operation_id')
})

test('a proposed change is never folded away with the work', () => {
  const markup = body({
    role: 'assistant',
    content: '',
    parts: [
      { type: 'tool', tool: command('a', 'ls', '{"exit_code":0}') },
      { type: 'text', text: 'I will create the project. Approve below.' },
      {
        type: 'tool',
        tool: {
          id: 'w',
          name: 'temps_write',
          arguments: '{}',
          result: 'usage: temps_write <method> <path>',
        },
      },
    ],
  })
  expect(markup).toContain('>Run<')
  expect(markup).toContain('I will create the project. Approve below.')
  expect(markup).toContain('temps_write')
})

test('two finished steps fold under one summary', () => {
  const markup = body({
    role: 'assistant',
    content: '',
    parts: [
      { type: 'tool', tool: command('a', 'ls', '{"exit_code":0}') },
      { type: 'tool', tool: command('b', 'pwd', '{"exit_code":0}') },
      { type: 'text', text: 'Both ran.' },
    ],
  })
  expect(markup).toContain('Ran 2 commands')
})

test('an answer without tools has no work group', () => {
  const markup = body({ role: 'assistant', content: 'You have two projects.' })
  expect(markup).toContain('You have two projects.')
  expect(markup).not.toContain('aria-expanded')
})
