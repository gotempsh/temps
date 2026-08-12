import { expect, test } from '../fixtures'
import type { Page } from '@playwright/test'
import path from 'node:path'

/**
 * Reliably capture the bytes of a `File`/`Blob` uploaded via `fetch(url, {
 * body: FormData })` to a URL containing `urlSubstring`, by reading it
 * directly in the browser before the request is serialized onto the wire.
 *
 * `route.request().postDataBuffer()` looks like the obvious way to inspect
 * an intercepted request's body, but for multipart bodies carrying binary
 * file content it is unreliable: Chromium's CDP `Fetch.requestPaused` event
 * (what `page.route()` interception is built on) does not always deliver the
 * complete `postData` for binary/multipart payloads under load, so the
 * captured buffer can come back truncated even though the request that
 * actually left the page was complete. Confirmed via the trace of a flaky CI
 * run: the trace's own network capture (a separate CDP `Network` domain
 * listener) showed the fully correct body while `postDataBuffer()` inside
 * the route handler did not contain it.
 *
 * Reading the `File` object directly inside the page sidesteps CDP body
 * capture entirely.
 */
async function captureUploadedFileBytes(
  page: Page,
  urlSubstring: string,
  fieldName = 'file'
): Promise<() => Buffer | null> {
  let captured: Buffer | null = null
  const exposedName = `__captureUploadedFile_${Math.random().toString(36).slice(2)}`
  await page.exposeFunction(exposedName, (base64: string) => {
    captured = Buffer.from(base64, 'base64')
  })
  await page.addInitScript(
    ({ exposedName, urlSubstring, fieldName }) => {
      const originalFetch = window.fetch
      const wrappedFetch = async (
        input: RequestInfo | URL,
        init?: RequestInit
      ): Promise<Response> => {
        const url =
          typeof input === 'string'
            ? input
            : input instanceof Request
              ? input.url
              : String(input)
        if (url.includes(urlSubstring) && init?.body instanceof FormData) {
          const file = init.body.get(fieldName)
          if (file instanceof Blob) {
            const buf = await file.arrayBuffer()
            const bytes = new Uint8Array(buf)
            let binary = ''
            for (let i = 0; i < bytes.length; i += 1) {
              binary += String.fromCharCode(bytes[i])
            }
            // @ts-expect-error injected via page.exposeFunction
            await window[exposedName](btoa(binary))
          }
        }
        return originalFetch(input, init)
      }
      window.fetch = Object.assign(wrappedFetch, originalFetch)
    },
    { exposedName, urlSubstring, fieldName }
  )
  return () => captured
}

test.describe('empty-state Drop handoff', () => {
  test('uploads a ZIP from the project card and starts detection on /drop', async ({
    page,
  }) => {
    await page.route(/\/api\/projects(?:\?.*)?$/, async (route) => {
      if (route.request().method() === 'GET') {
        await route.fulfill({
          json: { projects: [], page: 1, per_page: 20, total: 0 },
        })
        return
      }
      await route.fulfill({
        status: 201,
        json: { id: 42, name: 'browser-folder', slug: 'browser-folder' },
      })
    })
    await page.route('**/api/drop/inspect', async (route) => {
      await route.fulfill({
        json: {
          suggestedName: 'browser-folder',
          candidates: [
            {
              preset: 'static',
              label: 'Static HTML',
              directory: '.',
              confidence: 'high',
              reason: 'index.html found',
              isStatic: true,
            },
          ],
        },
      })
    })
    await page.route('**/api/projects/42/environments', async (route) => {
      await route.fulfill({
        json: [
          {
            id: 7,
            project_id: 42,
            name: 'production',
            slug: 'production',
            main_url: 'https://browser-folder.example.test',
            is_preview: false,
          },
        ],
      })
    })
    const getUploadedBytes = await captureUploadedFileBytes(
      page,
      '/api/projects/42/upload/static'
    )
    await page.route('**/api/projects/42/upload/static', async (route) => {
      await route.fulfill({
        status: 201,
        json: { id: 91, project_id: 42 },
      })
    })
    await page.route(
      '**/api/projects/42/environments/7/deploy/static',
      async (route) => {
        await route.fulfill({ status: 201, json: { id: 301 } })
      }
    )

    await page.goto('/projects')
    await expect(
      page.getByRole('heading', { name: 'Drop project files' })
    ).toBeVisible()

    const chooserPromise = page.waitForEvent('filechooser')
    await page.getByRole('button', { name: 'Upload ZIP' }).click()
    const chooser = await chooserPromise
    await chooser.setFiles({
      name: 'site.zip',
      mimeType: 'application/zip',
      buffer: Buffer.from('browser-test-archive'),
    })

    await page.waitForURL(/\/drop$/)
    await expect(
      page.getByText('Static HTML', { exact: true }).first()
    ).toBeVisible()
    await expect(page.getByLabel('Project name')).toHaveValue('browser-folder')
    await page.getByRole('button', { name: 'Deploy Static HTML' }).click()
    await expect(page.getByText('Drop accepted')).toBeVisible()
    expect(getUploadedBytes()?.toString()).toBe('browser-test-archive')
  })

  test('hands a selected folder to /drop with nested paths intact', async ({
    page,
  }) => {
    await page.route(/\/api\/projects(?:\?.*)?$/, async (route) => {
      if (route.request().method() !== 'GET') return route.continue()
      await route.fulfill({
        json: { projects: [], page: 1, per_page: 20, total: 0 },
      })
    })
    const getInspectedBytes = await captureUploadedFileBytes(
      page,
      '/api/drop/inspect'
    )
    await page.route('**/api/drop/inspect', async (route) => {
      await route.fulfill({
        json: {
          suggestedName: 'drop-folder-site',
          candidates: [
            {
              preset: 'static',
              label: 'Static HTML',
              directory: '.',
              confidence: 'high',
              reason: 'index.html found',
              isStatic: true,
            },
          ],
        },
      })
    })

    await page.goto('/projects')
    const chooserPromise = page.waitForEvent('filechooser')
    await page.getByRole('button', { name: 'Open folder' }).click()
    const chooser = await chooserPromise
    await chooser.setFiles(
      path.join(process.cwd(), 'e2e/fixtures/drop-folder-site')
    )

    await page.waitForURL(/\/drop$/)
    await expect(page.getByLabel('Project name')).toHaveValue(
      'drop-folder-site'
    )
    const archiveBytes = getInspectedBytes()
    expect(archiveBytes).not.toBeNull()
    const archiveContainsNestedPath =
      archiveBytes !== null &&
      archiveBytes.includes(Buffer.from('index.html')) &&
      archiveBytes.includes(Buffer.from('assets/app.js'))
    expect(archiveContainsNestedPath).toBe(true)
  })
})
