// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test, type Page } from '@playwright/test'

/**
 * *Make it reachable* is a page (ADR 069): the backend says first which
 * audiences can work, the URL carries the choice and the run, and Back walks
 * the choices. Fully route-mocked: nothing is created on the instance.
 */

const project = {
  id: 7301,
  name: 'Reach demo',
  slug: 'reach-demo',
  main_branch: 'main',
  directory: '.',
  source_type: 'docker_image',
  project_type: 'application',
  attack_mode: false,
  deployment_config: {},
  created_at: 0,
  updated_at: 0,
}

const environment = {
  id: 41,
  project_id: project.id,
  name: 'production',
  slug: 'production',
  subdomain: 'reach-demo-production',
  branch: 'main',
  domains: [],
  current_deployment_id: null,
  created_at: 0,
  updated_at: 0,
}

const controlPlane =
  'production runs on the control plane, and a tunnel only carries what workers serve. Deploy it to a worker to put it behind a tunnel.'

const choices = {
  project_id: project.id,
  environment_id: environment.id,
  environment_name: 'production',
  deployed: true,
  running_on: ['control plane'],
  audiences: [
    {
      audience: 'public',
      gate: null,
      available: true,
      blocked: null,
      suggested_address: null,
      ways_in: [
        {
          kind: 'direct_manual_dns',
          title: 'Straight to this server',
          way_in: 'direct:control-plane',
          recommended: true,
        },
      ],
      not_offered: [{ reason: controlPlane, action: null }],
    },
    {
      audience: 'signed_in',
      gate: 'temps',
      available: true,
      blocked: null,
      suggested_address: null,
      ways_in: [
        {
          kind: 'direct_manual_dns',
          title: 'Straight to this server',
          way_in: 'direct:control-plane',
          recommended: true,
        },
      ],
      not_offered: [],
    },
    {
      audience: 'signed_in',
      gate: 'cloudflare_access',
      available: false,
      blocked: {
        reason: 'Cloudflare Access sees only traffic through a tunnel.',
        action: null,
      },
      suggested_address: null,
      ways_in: [],
      not_offered: [],
    },
    {
      audience: 'private',
      available: false,
      gate: null,
      blocked: {
        reason: 'No worker listens on a private address yet.',
        action: {
          kind: 'run_command',
          label: 'On a worker, as root, run',
          href: null,
          command: 'temps agent ingress private',
        },
      },
      suggested_address: null,
      ways_in: [],
      not_offered: [],
    },
    {
      audience: 'tailnet_users',
      gate: null,
      available: false,
      blocked: {
        reason: 'No agent can tell who a tailnet visitor is.',
        action: null,
      },
      suggested_address: null,
      ways_in: [],
      not_offered: [],
    },
  ],
}

function planFor(audience: string, address: string | null) {
  const shown = address ?? 'the address'
  return {
    plan_id: `plan-${audience}-${address ?? 'none'}`,
    fingerprint: 'f',
    expires_at: '2026-10-28T12:15:00Z',
    audience,
    address,
    address_error: null,
    blocked: null,
    options: [
      {
        kind: 'direct_manual_dns',
        title: 'Straight to this server',
        description: `Visitors reach ${shown} on this server's public address.`,
        actions: 2,
        recommended: true,
        way_in: 'direct:control-plane',
        steps: [
          { kind: 'attach_domain', label: `Attach ${shown} to production` },
          { kind: 'show_dns_record', label: `Show the A record for ${shown}` },
          // A step kind this console has never seen renders like any other.
          {
            kind: 'enable_https_redirect',
            label: `Redirect http://${shown} to HTTPS`,
          },
          { kind: 'check_reachability', label: `Check ${shown}` },
        ],
        fields: [],
        warnings: [],
        confirmation: null,
        required_permissions: [],
      },
    ],
  }
}

const run = {
  id: 88,
  project_id: project.id,
  environment_id: environment.id,
  audience: 'public',
  address: 'app.example.com',
  option: 'direct_manual_dns',
  plan_id: 'plan-public-app.example.com',
  state: 'waiting',
  steps: [
    {
      kind: 'attach_domain',
      label: 'Attach app.example.com to production',
      status: 'done',
    },
    {
      kind: 'show_dns_record',
      label: 'Add an A record for app.example.com',
      status: 'done',
    },
    {
      kind: 'check_reachability',
      label: 'Check app.example.com',
      status: 'running',
    },
  ],
  created: [],
  journal: [],
  undo: [],
  error: null,
  live_url: null,
  created_at: '2026-10-28T12:00:00Z',
  updated_at: '2026-10-28T12:00:05Z',
  finished_at: null,
}

async function mockReachability(page: Page) {
  const applied: unknown[] = []
  let runs: unknown[] = []
  await page.route(`**/api/projects/by-slug/${project.slug}`, (route) =>
    route.fulfill({ json: project })
  )
  await page.route(`**/api/projects/${project.id}/environments`, (route) =>
    route.fulfill({ json: [environment] })
  )
  const base = `**/api/projects/${project.id}/environments/${environment.id}/reachability`
  await page.route(base, (route) =>
    route.fulfill({
      json: {
        project_id: project.id,
        environment_id: environment.id,
        environment_name: 'production',
        deployed: true,
        running_on: ['control plane'],
        state: 'not_reachable',
        summary: 'Deployed on the control plane, but nobody can reach it yet.',
        addresses: [
          {
            address: 'reach-demo-production.localho.st',
            audience: 'public',
            source: 'environment_domain',
            custom_domain_id: null,
            way_in: null,
            state: 'not_reachable',
            reason: 'Only resolves on this machine.',
            action: null,
          },
        ],
        plan: null,
        primary_address: null,
      },
    })
  )
  await page.route(`${base}/choices`, (route) =>
    route.fulfill({ json: choices })
  )
  await page.route(`${base}/runs`, (route) => route.fulfill({ json: runs }))
  await page.route(`${base}/runs/${run.id}`, (route) =>
    route.fulfill({ json: run })
  )
  await page.route(`${base}/plan`, (route) => {
    const body = route.request().postDataJSON() as {
      audience: string
      address: string | null
    }
    return route.fulfill({ json: planFor(body.audience, body.address) })
  })
  await page.route(`${base}/apply`, (route) => {
    applied.push(route.request().postDataJSON())
    runs = [run]
    return route.fulfill({ status: 202, json: run })
  })
  return { applied }
}

/** The project layout loads several queries before the page; give it time. */
async function expectPage(page: Page) {
  await expect(
    page.getByRole('heading', { name: 'Make it reachable', level: 1 })
  ).toBeVisible({ timeout: 60_000 })
}

const pagePath = `/projects/${project.slug}/environments/${environment.id}/reachability`

test('offers only what can work, says why the rest cannot, and links the run', async ({
  page,
}) => {
  const { applied } = await mockReachability(page)
  await page.goto(pagePath)

  await expectPage(page)
  const audiences = page.getByRole('radiogroup', { name: 'Who can open it' })
  // Options that cannot work are shown, disabled, with the backend's reason.
  await expect(
    audiences.getByRole('radio', { name: 'Only my private network' })
  ).toBeDisabled()
  await expect(
    page.getByText('No worker listens on a private address yet.')
  ).toBeVisible()
  await expect(page.getByText('temps agent ingress private')).toBeVisible()
  await expect(
    audiences.getByRole('radio', { name: 'Anyone on the internet' })
  ).toBeChecked()
  // Why there is no tunnel, before an address is typed.
  await expect(page.getByText(controlPlane)).toBeVisible()

  // Choosing an audience is a history entry; Back undoes it.
  await audiences.getByRole('radio', { name: 'People who sign in' }).click()
  await expect(page).toHaveURL(/audience=signed_in/)
  await expect(
    page
      .getByRole('radiogroup', { name: 'Sign in with' })
      .getByRole('radio', { name: 'Cloudflare Access' })
  ).toBeDisabled()
  await page.goBack()
  await expect(page).not.toHaveURL(/audience=signed_in/)
  await expect(
    audiences.getByRole('radio', { name: 'Anyone on the internet' })
  ).toBeChecked()

  await page.getByLabel('Address', { exact: true }).fill('app.example.com')
  await expect(page).toHaveURL(/address=app\.example\.com/)
  const steps = page.getByRole('list', { name: 'What Temps will do' })
  await expect(steps.getByRole('listitem')).toHaveText([
    /Attach app\.example\.com to production/,
    /Show the A record for app\.example\.com/,
    /Redirect http:\/\/app\.example\.com to HTTPS/,
    /Check app\.example\.com/,
  ])
  await expect(
    page.getByText('reach-demo-production.localho.st', { exact: false })
  ).toBeVisible()

  await page.getByRole('button', { name: 'Straight to this server' }).click()
  await expect(page).toHaveURL(new RegExp(`run=${run.id}`))
  expect(applied).toEqual([
    expect.objectContaining({
      plan_id: 'plan-public-app.example.com',
      option: 'direct_manual_dns',
    }),
  ])
  await expect(
    page.getByRole('list', { name: 'Progress' }).getByRole('listitem')
  ).toHaveCount(3)
  await expect(
    page.getByText('Waiting for the address to answer')
  ).toBeVisible()

  // Back leaves the run for the plan, which now points at the unfinished run.
  await page.goBack()
  await expect(page).not.toHaveURL(/run=/)
  await page.getByRole('link', { name: 'Open the setup' }).click()
  await expect(page).toHaveURL(new RegExp(`run=${run.id}`))
})

test('a deep link opens the audience it names, or says why it cannot', async ({
  page,
}) => {
  await mockReachability(page)
  await page.goto(`${pagePath}?audience=private&address=lan.example.com`)
  await expectPage(page)
  await expect(
    page.getByText(
      /The link asked for “Only my private network”, which cannot work for production yet/
    )
  ).toBeVisible()
  await expect(page.getByLabel('Address', { exact: true })).toHaveValue(
    'lan.example.com'
  )

  // A hand-typed environment name resolves to its id, keeping the query.
  await page.goto(
    `/projects/${project.slug}/environments/production/reachability?audience=signed_in`
  )
  await expectPage(page)
  await expect(page).toHaveURL(
    new RegExp(
      `/environments/${environment.id}/reachability\\?audience=signed_in`
    )
  )
  await expect(
    page
      .getByRole('radiogroup', { name: 'Who can open it' })
      .getByRole('radio', { name: 'People who sign in' })
  ).toBeChecked()
})
