import { expect, expectAppMounted, test, urlForProject } from '../fixtures'

/**
 * Creates a project from a PUBLIC git repository, entirely through the UI.
 *
 * This is the flow a first-time self-hosted user hits, and it is the one with
 * no shortcut available: it needs no Git provider connection, no OAuth, no
 * token -- just a URL. That makes it the only end-to-end deploy path that is
 * honestly testable in CI without provisioning credentials.
 *
 * Server-side this clones the repository and runs preset detection, so it is
 * the slowest spec here. It is also the one that proves the console, the API
 * and the git layer agree with each other.
 */

// The temps repository itself: public, always available, and already what the
// API-level half of e2e-tests.yml deploys from.
const PUBLIC_REPO_URL =
  process.env.E2E_REPO_URL ?? 'https://github.com/gotempsh/temps.git'

/**
 * Unique per run AND per retry.
 *
 * The retry index matters: if an attempt creates the project and then fails
 * later in the flow, a retry reusing the same name hits a duplicate-name error
 * and fails permanently -- turning a transient problem into a hard red.
 */
function projectName(retry: number): string {
  const run = process.env.GITHUB_RUN_ID ?? String(process.hrtime.bigint())
  return `e2e-ui-${run.slice(-8)}-${retry}`.toLowerCase().slice(0, 32)
}

test.describe('project creation', () => {
  test('creates a project from a public git URL', async ({
    page,
    consoleErrors,
  }, testInfo) => {
    const name = projectName(testInfo.retry)

    await page.goto('/projects/new')
    await expectAppMounted(page)
    await expect(
      page.getByRole('heading', { name: 'New Project' })
    ).toBeVisible()

    // Source-type switcher. These are plain buttons rather than ARIA tabs, so
    // they're addressed by their accessible name.
    await page.getByRole('button', { name: 'Git URL', exact: true }).click()

    const urlField = page.getByLabel('Public Repository URL')
    await expect(urlField).toBeVisible()
    await urlField.fill(PUBLIC_REPO_URL)

    await page.getByRole('button', { name: 'Continue with URL' }).click()

    // The backend clones and inspects the repo here; give it room. Reaching the
    // config step at all proves the clone and preset detection succeeded.
    const nameField = page.getByLabel('Project Name')
    await expect(
      nameField,
      'the repo should be cloned and inspected'
    ).toBeVisible({
      timeout: 90_000,
    })
    // Branch and Framework Preset are custom comboboxes rather than labelled
    // form controls, so they are asserted by their visible text, not getByLabel.
    await expect(page.getByText('Branch', { exact: true })).toBeVisible()
    await expect(
      page.getByText('Root Directory', { exact: true })
    ).toBeVisible()

    // Preset auto-detection resolves a beat AFTER the form paints, and until it
    // does no preset is selected. Submitting in that window fails zod
    // validation on `preset`, so the click is a silent no-op: no navigation,
    // nothing visible, just `Form validation errors: {preset: ...}` on the
    // console. Waiting on the name field alone is therefore not enough -- this
    // is what made the spec flaky in CI, where the first (cold) detection is
    // slow and only the warm retry passed.
    await expect(
      page.getByText(/We detected the following presets/i),
      'preset auto-detection must finish before submitting, or the form silently refuses'
    ).toBeVisible({ timeout: 90_000 })

    await nameField.fill(name)
    await page
      .getByRole('button', { name: 'Create Project', exact: true })
      .click()

    // Landing on the project page is the success signal.
    await page.waitForURL(urlForProject(name), { timeout: 60_000 })
    await expectAppMounted(page)
    await expect(
      page.getByRole('heading', { name, exact: true }).first()
    ).toBeVisible()

    // And it must survive a reload -- i.e. it was actually persisted, not just
    // rendered optimistically from local state.
    await page.goto('/projects')
    await expect(page.getByText(name, { exact: true }).first()).toBeVisible({
      timeout: 30_000,
    })

    expect(consoleErrors).toEqual([])
  })

  test('surfaces a clear error for an unreachable repository', async ({
    page,
  }) => {
    // Failure paths matter more than usual here: a self-hosted user debugging a
    // bad URL has no support channel, so "nothing happened" is unacceptable.
    await page.goto('/projects/new')
    await page.getByRole('button', { name: 'Git URL', exact: true }).click()

    await page
      .getByLabel('Public Repository URL')
      .fill(
        'https://github.com/gotempsh/this-repository-does-not-exist-e2e.git'
      )
    await page.getByRole('button', { name: 'Continue with URL' }).click()

    // Either an explicit error, or at minimum the form does not silently
    // pretend it worked by advancing to the configuration step.
    const errorMessage = page
      .getByText(/not found|could not|failed|unable|does not exist/i)
      .first()
    await expect(
      errorMessage,
      'an unreachable repository should produce a visible, actionable error rather than a silent no-op'
    ).toBeVisible({ timeout: 90_000 })
  })
})
