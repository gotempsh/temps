// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test, type Page } from '@playwright/test'

const project = {
  id: 71,
  name: 'Trace demo',
  slug: 'trace-demo',
  main_branch: 'main',
  directory: '.',
  source_type: 'docker_image',
  project_type: 'application',
  attack_mode: false,
  deployment_config: {},
  created_at: 0,
  updated_at: 0,
}
const trace = {
  trace_id: '0123456789abcdef0123456789abcdef',
  root_span_name: 'chat synthetic-model',
  service_name: 'demo-api',
  start_time: '2026-08-04T12:00:00.000Z',
  duration_ms: 12_000,
  span_count: 1,
  error_count: 0,
  gen_ai_system: 'openai',
  gen_ai_model: 'synthetic-model',
  gen_ai_operation: 'chat',
  total_input_tokens: 24,
  total_output_tokens: 12,
}
const bounds = {
  start_time: '2026-08-04T11:55:00.000Z',
  end_time: '2026-08-04T12:05:12.000Z',
}

async function mockProject(page: Page) {
  await page.route('**/api/projects?*', (route) =>
    route.fulfill({ json: { projects: [project], total: 1 } })
  )
  await page.route('**/api/projects', (route) =>
    route.fulfill({ json: { projects: [project], total: 1 } })
  )
  await page.route('**/api/projects/by-slug/trace-demo', (route) =>
    route.fulfill({ json: project })
  )
  await page.route('**/api/projects/71/environments', (route) =>
    route.fulfill({ json: [] })
  )
  await page.route('**/api/otel/has-traces/71', (route) =>
    route.fulfill({ json: { has_traces: true } })
  )
}

function expectBounds(url: string) {
  const params = new URL(url).searchParams
  expect(params.get('start_time')).toBe(bounds.start_time)
  expect(params.get('end_time')).toBe(bounds.end_time)
}

test.beforeEach(async ({ page }) => {
  await mockProject(page)
})

test('trace list opens historical detail with timestamp bounds', async ({
  page,
}) => {
  await page.route('**/api/otel/trace-summaries?*', (route) =>
    route.fulfill({ json: { data: [trace], total: 1 } })
  )
  await page.route('**/api/otel/traces/71/*', (route) =>
    route.fulfill({ json: { data: [], count: 0 } })
  )
  await page.goto('/projects/trace-demo/traces')
  const request = page.waitForRequest(
    (req) =>
      new URL(req.url()).pathname === `/api/otel/traces/71/${trace.trace_id}`
  )
  await page.getByText(trace.root_span_name, { exact: true }).click()
  expectBounds((await request).url())
  expectBounds(page.url())
  await page.screenshot({
    path: '/tmp/cloud-trace-detail-bounds.png',
    fullPage: true,
  })
})

test('AI activity preserves detail timestamps through reload and back navigation', async ({
  page,
}) => {
  await page.route('**/api/otel/genai/traces?*', (route) =>
    route.fulfill({ json: { data: [trace], total: 1 } })
  )
  await page.route('**/api/otel/genai/traces/71/*', (route) =>
    route.fulfill({
      json: {
        trace_id: trace.trace_id,
        span_count: 1,
        events: [],
        event_count: 0,
        spans: [
          {
            span_id: '0123456789abcdef',
            parent_span_id: null,
            name: trace.root_span_name,
            kind: 'client',
            start_time: trace.start_time,
            duration_ms: trace.duration_ms,
            status_code: 'OK',
            attributes: {},
            gen_ai_system: 'openai',
            gen_ai_operation: 'chat',
            gen_ai_model: 'synthetic-model',
            input_tokens: 24,
            output_tokens: 12,
          },
        ],
      },
    })
  )
  await page.goto('/projects/trace-demo/ai-gateway?keep=example')
  const request = page.waitForRequest(
    (req) =>
      new URL(req.url()).pathname ===
      `/api/otel/genai/traces/71/${trace.trace_id}`
  )
  await page.getByText(trace.root_span_name, { exact: true }).click()
  expectBounds((await request).url())
  expectBounds(page.url())
  await expect(page.getByText('Total Spans', { exact: true })).toBeVisible()
  const reloaded = page.waitForRequest(
    (req) =>
      new URL(req.url()).pathname ===
      `/api/otel/genai/traces/71/${trace.trace_id}`
  )
  await page.reload()
  expectBounds((await reloaded).url())
  await expect(page.getByText('Total Spans', { exact: true })).toBeVisible()
  await page.screenshot({
    path: '/tmp/cloud-ai-trace-detail.png',
    fullPage: true,
  })
  await page.getByRole('button', { name: 'Back to traces' }).click()
  const params = new URL(page.url()).searchParams
  expect(params.get('keep')).toBe('example')
  expect(params.has('trace')).toBe(false)
  expect(params.has('start_time')).toBe(false)
})

test('AI storage failures show an error instead of an empty onboarding state', async ({
  page,
}) => {
  await page.route('**/api/otel/genai/traces?*', (route) =>
    route.fulfill({
      status: 500,
      json: { title: 'Internal Server Error', detail: 'Storage unavailable' },
    })
  )
  await page.goto('/projects/trace-demo/ai-gateway')
  await expect(
    page.getByText('Unable to load AI traces', { exact: true })
  ).toBeVisible({ timeout: 30_000 })
  await page.screenshot({
    path: '/tmp/cloud-ai-traces-error.png',
    fullPage: true,
  })
})

test('empty Cloud allowlist has an explicit metadata opt-in that preserves other keys', async ({
  page,
}) => {
  let settings = {
    project_id: 71,
    write_mode: 'cloud',
    effective_write_mode: 'cloud',
    fidelity: 'queryable',
    attribute_allowlist: ['custom.safe'],
    analytics_write_mode: 'local',
    cloud_write_mode_available: true,
    queued_spans: 0,
    dead_lettered_spans: 0,
    gap_windows: [],
    intervals: [],
  }
  let saves = 0
  await page.route('**/api/otel/cloud-telemetry/projects/71', async (route) => {
    if (route.request().method() === 'PATCH') {
      saves++
      const body = route.request().postDataJSON()
      expect(body.attribute_allowlist).toContain('custom.safe')
      expect(body.attribute_allowlist).toContain('gen_ai.provider.name')
      expect(body.attribute_allowlist).toContain('gen_ai.usage.input_tokens')
      expect(body.attribute_allowlist).not.toContain('gen_ai.input.messages')
      expect(body.attribute_allowlist).not.toContain('gen_ai.output.messages')
      settings = { ...settings, ...body }
    }
    await route.fulfill({ json: settings })
  })
  await page.route('**/api/otel/genai/traces?*', (route) =>
    route.fulfill({ json: { data: [], total: 0 } })
  )
  await page.goto('/projects/trace-demo/ai-gateway')
  await expect(
    page.getByText('AI metadata is not fully enabled for Cloud', {
      exact: true,
    })
  ).toBeVisible()
  await page.getByRole('link', { name: 'Configure AI metadata' }).click()
  const checkbox = page.getByRole('checkbox', {
    name: 'Allow AI metadata in Cloud',
  })
  await expect(checkbox).not.toBeChecked()
  expect(saves).toBe(0)
  await checkbox.check()
  expect(saves).toBe(0)
  await page.screenshot({
    path: '/tmp/cloud-ai-metadata-consent.png',
    fullPage: true,
  })
  const saved = page.waitForResponse(
    (response) =>
      response.request().method() === 'PATCH' &&
      response.url().includes('/otel/cloud-telemetry/projects/71')
  )
  await page.getByRole('button', { name: 'Save changes', exact: true }).click()
  expect((await saved).ok()).toBe(true)
  expect(saves).toBe(1)
  await page.goto('/projects/trace-demo/ai-gateway')
  await expect(
    page.getByText('AI metadata is not fully enabled for Cloud', {
      exact: true,
    })
  ).toHaveCount(0)
})

test('AI metadata save merges the latest server allowlist', async ({
  page,
}) => {
  let settings = {
    project_id: 71,
    write_mode: 'cloud',
    effective_write_mode: 'cloud',
    fidelity: 'queryable',
    attribute_allowlist: ['custom.removed'],
    analytics_write_mode: 'local',
    cloud_write_mode_available: true,
    queued_spans: 0,
    dead_lettered_spans: 0,
    gap_windows: [],
    intervals: [],
  }
  let gets = 0
  let patches = 0
  await page.route('**/api/otel/cloud-telemetry/projects/71', async (route) => {
    if (route.request().method() === 'GET') {
      gets++
    } else if (route.request().method() === 'PATCH') {
      patches++
      const body = route.request().postDataJSON()
      expect(gets).toBeGreaterThan(1)
      expect(body.attribute_allowlist).toContain('custom.added')
      expect(body.attribute_allowlist).not.toContain('custom.removed')
      expect(body.attribute_allowlist).toContain('gen_ai.provider.name')
      expect(body.write_mode).toBeUndefined()
      expect(body.fidelity).toBeUndefined()
      settings = { ...settings, ...body }
    }
    await route.fulfill({ json: settings })
  })
  await page.goto('/projects/trace-demo/settings/telemetry')
  const checkbox = page.getByRole('checkbox', {
    name: 'Allow AI metadata in Cloud',
  })
  await expect(checkbox).toBeVisible()
  await checkbox.check()
  settings = { ...settings, attribute_allowlist: ['custom.added'] }
  await page.getByRole('button', { name: 'Save changes', exact: true }).click()
  await expect.poll(() => patches).toBe(1)
})

test('AI metadata save stops when the fresh settings read fails', async ({
  page,
}) => {
  const settings = {
    project_id: 71,
    write_mode: 'cloud',
    effective_write_mode: 'cloud',
    fidelity: 'queryable',
    attribute_allowlist: [],
    analytics_write_mode: 'local',
    cloud_write_mode_available: true,
    queued_spans: 0,
    dead_lettered_spans: 0,
    gap_windows: [],
    intervals: [],
  }
  let gets = 0
  let patches = 0
  await page.route('**/api/otel/cloud-telemetry/projects/71', async (route) => {
    if (route.request().method() === 'PATCH') {
      patches++
      await route.fulfill({ json: settings })
    } else if (++gets > 1) {
      await route.fulfill({
        status: 503,
        json: { detail: 'Settings unavailable' },
      })
    } else {
      await route.fulfill({ json: settings })
    }
  })
  await page.goto('/projects/trace-demo/settings/telemetry')
  const checkbox = page.getByRole('checkbox', {
    name: 'Allow AI metadata in Cloud',
  })
  await expect(checkbox).toBeVisible()
  await checkbox.check()
  await page.getByRole('button', { name: 'Save changes', exact: true }).click()
  await expect.poll(() => gets).toBeGreaterThan(1)
  await expect(
    page.getByRole('button', { name: 'Save changes', exact: true })
  ).toBeEnabled()
  expect(patches).toBe(0)
})

test('automatic cross-project detail carries the same historical window', async ({
  page,
}) => {
  const span = {
    trace_id: trace.trace_id,
    span_id: '0123456789abcdef',
    project_id: 71,
    parent_span_id: null,
    name: trace.root_span_name,
    kind: 'client',
    start_time: trace.start_time,
    end_time: '2026-08-04T12:00:12.000Z',
    duration_ms: 12_000,
    status_code: 'OK',
    status_message: '',
    attributes: {},
    events: [],
    resource: { service_name: 'demo-api', attributes: {} },
  }
  await page.route('**/api/otel/traces/71/*', (route) =>
    route.fulfill({ json: { data: [span], count: 1 } })
  )
  await page.route('**/api/otel/traces/cross-project/*', (route) =>
    route.fulfill({
      json: {
        siblings: [
          {
            project_id: 72,
            project_name: 'Worker demo',
            project_slug: 'worker-demo',
            first_seen: trace.start_time,
          },
        ],
      },
    })
  )
  await page.route('**/api/otel/global/traces/*', (route) =>
    route.fulfill({
      json: {
        trace_id: trace.trace_id,
        projects: [
          {
            project_id: 71,
            project_name: project.name,
            project_slug: project.slug,
          },
        ],
        spans: [{ project_id: 71, project_name: project.name, span }],
        start_time: trace.start_time,
        end_time: span.end_time,
        total_duration_ms: 12_000,
        span_count: 1,
        error_count: 0,
        has_redacted_spans: false,
        truncated: false,
        truncated_projects: [],
      },
    })
  )
  const request = page.waitForRequest(
    (req) =>
      new URL(req.url()).pathname ===
      `/api/otel/global/traces/${trace.trace_id}`
  )
  await page.goto(
    `/projects/trace-demo/traces/${trace.trace_id}?${new URLSearchParams(bounds)}`
  )
  expectBounds((await request).url())
  await expect(
    page.getByText('This trace also has spans in:', { exact: true })
  ).toBeVisible()
  const sibling = page.getByRole('link', { name: 'Worker demo', exact: true })
  expectBounds(new URL((await sibling.getAttribute('href'))!, page.url()).href)
})
