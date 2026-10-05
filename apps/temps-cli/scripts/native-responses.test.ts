// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { createClient } from '../src/api/client'
import { createResponseJson, createResponseStream } from '../src/api/sdk.gen'

test('generated Responses SSE binding yields an event before the body closes', async () => {
  let controller!: ReadableStreamDefaultController<Uint8Array>
  let closed = false
  const body = new ReadableStream<Uint8Array>({
    start(value) { controller = value },
  })
  const encoder = new TextEncoder()
  const fetch = (async (request: Request) => {
    expect(request.url).toBe('https://provider.example/ai/v1/responses/stream')
    return new Response(body, { headers: { 'Content-Type': 'text/event-stream' } })
  }) as typeof globalThis.fetch
  const client = createClient({ baseUrl: 'https://provider.example', fetch })
  const result = await createResponseStream({ body: { model: 'test-model', stream: true }, client })
  const events = result.stream[Symbol.asyncIterator]()
  controller.enqueue(encoder.encode('data: {"type":"response.output_text.delta","delta":"hello"}\n\n'))
  const first = await events.next()
  expect(first.value).toEqual({ type: 'response.output_text.delta', delta: 'hello' })
  expect(first.done).toBe(false)
  if (first.done) throw new Error('stream closed before its first event')
  const eventType: string = first.value.type
  expect(eventType).toBe('response.output_text.delta')
  expect(closed).toBe(false)
  controller.enqueue(encoder.encode('data: {"type":"response.completed"}\n\n'))
  controller.close()
  closed = true
  expect((await events.next()).value).toEqual({ type: 'response.completed' })
  expect((await events.next()).done).toBe(true)
}, 2000)

test('generated Responses JSON binding returns a typed response object', async () => {
  const fetch = (async (request: Request) => {
    expect(request.url).toBe('https://provider.example/ai/v1/responses/json')
    return Response.json({ id: 'response-test', object: 'response', model: 'test-model' })
  }) as typeof globalThis.fetch
  const client = createClient({ baseUrl: 'https://provider.example', fetch })
  const result = await createResponseJson({ body: { model: 'test-model', stream: false }, client })
  expect(result.data?.id).toBe('response-test')
})
