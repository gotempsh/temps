import { expect, test } from '../fixtures'
import path from 'node:path'

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
    let staticUploadWasMultipart = false
    await page.route('**/api/projects/42/upload/static', async (route) => {
      const request = route.request()
      staticUploadWasMultipart =
        request
          .headers()
          ['content-type']?.startsWith('multipart/form-data;') === true &&
        request.postDataBuffer()?.includes(Buffer.from('name="file"')) === true
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
    expect(staticUploadWasMultipart).toBe(true)
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
    let archiveContainsNestedPath = false
    await page.route('**/api/drop/inspect', async (route) => {
      const body = route.request().postDataBuffer()
      archiveContainsNestedPath =
        body?.includes(Buffer.from('index.html')) === true &&
        body.includes(Buffer.from('assets/app.js'))
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
    expect(archiveContainsNestedPath).toBe(true)
  })
})
