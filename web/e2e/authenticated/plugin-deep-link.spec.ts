// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from '../fixtures'

/**
 * An external plugin's own route must survive a copy/paste of the address bar.
 *
 * Before this, every plugin URL collapsed to `/plugins/<name>`: the iframe
 * `src` was fixed, nothing mirrored the plugin's route outward, and the splat
 * in `<Route path="/plugins/:pluginName/*">` was declared but unread. Sharing
 * "look at this app" meant sharing a screenshot and directions.
 *
 * These specs test the console's half of the contract, not any one plugin's
 * router. They drive the iframe's `location.hash` directly rather than
 * clicking plugin UI, so they stay meaningful when the installed plugin
 * changes — and they skip cleanly on an instance with no plugins at all,
 * which is the default and must not read as a failure.
 */

interface PluginManifest {
  name: string
  nav: { section: string }[]
}

/** The first plugin with a platform-section nav entry, or null. */
async function platformPlugin(
  request: import('@playwright/test').APIRequestContext
): Promise<string | null> {
  const response = await request.get('/api/x/plugins')
  if (!response.ok()) return null
  const plugins = (await response.json()) as PluginManifest[]
  const found = plugins.find((plugin) =>
    plugin.nav?.some((entry) => entry.section === 'platform')
  )
  return found?.name ?? null
}

/**
 * The plugin's iframe, once it has actually loaded a document.
 *
 * `contentWindow` exists from the moment the element is parsed, pointing at
 * `about:blank`, so waiting on the element alone races the load and reads a
 * hash that is always empty.
 */
async function pluginFrame(
  page: import('@playwright/test').Page,
  name: string
) {
  const iframe = page.locator('iframe')
  await expect(iframe).toHaveCount(1)
  const frame = await page
    .frameLocator('iframe')
    .locator('body')
    .waitFor({ state: 'attached', timeout: 30_000 })
    .then(() =>
      page
        .frames()
        .find((candidate) => candidate.url().includes(`/api/x/${name}/`))
    )
  expect(frame, `the ${name} plugin iframe should have loaded`).toBeTruthy()
  return frame!
}

test.describe('external plugin deep linking', () => {
  test('a plugin sub-route is restored from a pasted console URL', async ({
    page,
    request,
  }) => {
    const name = await platformPlugin(request)
    test.skip(
      !name,
      'no external plugin with a platform nav entry is installed'
    )

    // A route the plugin need not recognise: what is under test is that the
    // console hands it over, not what the plugin renders for it.
    await page.goto(`/plugins/${name}/deep/link/probe`)

    const iframe = page.locator('iframe')
    await expect(iframe).toHaveCount(1)

    // The `src` is built once, at mount, and is the deterministic evidence:
    // it carries the pasted sub-route into the plugin. Asserting the frame's
    // live hash instead would race the plugin's own boot-time redirects.
    await expect(iframe).toHaveAttribute(
      'src',
      `/api/x/${name}/ui/#/deep/link/probe`
    )
  })

  test("the plugin's own route is mirrored into the console URL", async ({
    page,
    request,
  }) => {
    const name = await platformPlugin(request)
    test.skip(
      !name,
      'no external plugin with a platform nav entry is installed'
    )

    await page.goto(`/plugins/${name}`)
    const frame = await pluginFrame(page, name!)

    await frame.evaluate(() => {
      window.location.hash = '#/mirrored/route'
    })

    // The invariant is that the two agree — polled against whatever hash the
    // plugin settles on, since a plugin is entitled to redirect on arrival and
    // pinning the literal route would flake on that.
    await expect
      .poll(
        async () => {
          const hash = await frame.evaluate(() => window.location.hash)
          const subPath = hash.replace(/^#\/?/, '')
          return `${new URL(page.url()).pathname}|${subPath}`
        },
        {
          timeout: 15_000,
          message: 'the console URL should mirror the plugin route',
        }
      )
      .toMatch(new RegExp(`^/plugins/${name}/(.+)\\|\\1$`))

    // ...and it must have actually moved off the bare plugin route, or the
    // regex above would be satisfied by nothing having happened.
    expect(new URL(page.url()).pathname).not.toBe(`/plugins/${name}`)
  })

  test('mirroring a route never reloads the plugin', async ({
    page,
    request,
  }) => {
    const name = await platformPlugin(request)
    test.skip(
      !name,
      'no external plugin with a platform nav entry is installed'
    )

    await page.goto(`/plugins/${name}`)
    const frame = await pluginFrame(page, name!)

    // Anything on the plugin's `window` is lost if the document reloads. This
    // stands in for what a user would lose: an in-flight chat, a half-filled
    // form, a running build.
    await frame.evaluate(() => {
      ;(window as unknown as Record<string, unknown>).__e2eSurvives = 'yes'
    })

    for (const route of ['#/one', '#/two', '#/three']) {
      await frame.evaluate((target) => {
        window.location.hash = target
      }, route)
      await page.waitForTimeout(250)
    }

    const survived = await frame.evaluate(
      () => (window as unknown as Record<string, unknown>).__e2eSurvives
    )
    expect(
      survived,
      'the iframe was reloaded — mirroring must write location.hash, never src'
    ).toBe('yes')

    // The same guarantee stated structurally: `src` is written once.
    await expect(page.locator('iframe')).toHaveAttribute(
      'src',
      `/api/x/${name}/ui/`
    )
  })

  test('a plugin can drive the console URL over postMessage', async ({
    page,
    request,
  }) => {
    const name = await platformPlugin(request)
    test.skip(
      !name,
      'no external plugin with a platform nav entry is installed'
    )

    await page.goto(`/plugins/${name}`)
    const frame = await pluginFrame(page, name!)

    // The escape hatch for plugins that route without a hash.
    await frame.evaluate(() => {
      window.parent.postMessage(
        { type: 'temps:route', path: 'posted/route' },
        window.location.origin
      )
    })

    await page.waitForURL(
      new RegExp(`/plugins/${name}/posted/route(?:[?#]|$)`),
      { timeout: 15_000 }
    )

    // ...and it has to *stay*. A plugin routing by message leaves its hash
    // permanently empty, and the hash listener re-snapshots that empty value
    // whenever it re-attaches — which silently reset the URL to the bare
    // plugin route a beat after the message landed. Caught only because this
    // spec ran alongside others; alone, the re-attach never happened.
    await page.waitForTimeout(2_000)
    expect(new URL(page.url()).pathname).toBe(`/plugins/${name}/posted/route`)
  })

  test('a message that is not the route contract is ignored', async ({
    page,
    request,
  }) => {
    const name = await platformPlugin(request)
    test.skip(
      !name,
      'no external plugin with a platform nav entry is installed'
    )

    await page.goto(`/plugins/${name}`)
    const frame = await pluginFrame(page, name!)
    const before = page.url()

    // Shapes the console must not act on: an unrelated message type, and a
    // well-formed one carrying a non-string path. The address bar is a
    // capability — anything that can write it needs to be exactly our contract.
    await frame.evaluate(() => {
      window.parent.postMessage({ type: 'something:else', path: 'nope' }, '*')
      window.parent.postMessage({ type: 'temps:route', path: 42 }, '*')
      window.parent.postMessage('temps:route', '*')
    })

    await page.waitForTimeout(1_000)
    expect(page.url()).toBe(before)
  })
})
