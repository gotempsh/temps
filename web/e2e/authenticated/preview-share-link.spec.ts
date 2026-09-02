// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from '../fixtures'
import type { Page } from '@playwright/test'

async function navigateToNewPreviewRoute(
  page: Page,
  url: string,
  bridgePostCount: () => number
) {
  for (let attempt = 0; attempt < 3; attempt += 1) {
    try {
      await page.goto(url)
      return
    } catch (error) {
      const networkChanged =
        error instanceof Error && error.message.includes('ERR_NETWORK_CHANGED')

      if (!networkChanged) {
        throw error
      }

      // Chromium can invalidate its network context while the newly-created
      // wildcard preview route becomes available. If the one-time grant was
      // already submitted, do not submit it twice; the assertions below will
      // wait for and validate the completed navigation.
      if (bridgePostCount() > 0) {
        return
      }

      if (attempt === 2) {
        throw new Error(
          'The newly-created preview route remained unavailable after 3 navigation attempts',
          { cause: error }
        )
      }

      await page.waitForTimeout(250 * 2 ** attempt)
    }
  }
}

test.describe('sandbox preview share links', () => {
  test('requires an authenticated control-plane session', async ({
    browser,
  }) => {
    const baseURL = process.env.E2E_BASE_URL ?? 'http://localhost:8081'
    const anonymous = await browser.newContext()
    try {
      const page = await anonymous.newPage()
      await page.goto(`${baseURL.replace(/\/$/, '')}/login`)
      const status = await page.evaluate(async () => {
        const response = await fetch(
          '/api/v1/sandboxes/sbx_0000000000000000/preview-link',
          {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ port: 3000 }),
          }
        )
        return response.status
      })
      // Browser-origin requests may be concealed as 404 by the admin gate;
      // either response proves the credential-minting handler is unreachable.
      expect([401, 404]).toContain(status)
    } finally {
      await anonymous.close()
    }
  })

  test('keeps the grant in the fragment, auto-submits it, and scrubs browser history', async ({
    page,
  }) => {
    const created = await page.request.post('/api/v1/sandboxes', {
      data: {
        name: `preview-share-e2e-${Date.now()}`,
        timeout_secs: 600,
        preview_password: 'PreviewShareE2e!16',
        ports: [3000],
        backend: 'docker',
      },
    })
    expect(created.status()).toBe(201)
    const createdBody = (await created.json()) as {
      sandbox: { id: string }
    }
    const sandboxId = createdBody.sandbox.id

    try {
      const started = await page.request.post(
        `/api/v1/sandboxes/${sandboxId}/exec-detached`,
        {
          data: {
            cmd: [
              'node',
              '-e',
              'require("http").createServer((req,res)=>res.end("preview-share-ok:"+req.url)).listen(3000,"0.0.0.0")',
            ],
          },
        }
      )
      expect(started.status()).toBe(202)

      const ready = await page.request.post(
        `/api/v1/sandboxes/${sandboxId}/exec`,
        {
          data: {
            cmd: [
              'node',
              '-e',
              'const http=require("http");let attempts=0;(function ping(){http.get("http://127.0.0.1:3000/__ready",res=>process.exit(res.statusCode===200?0:1)).on("error",()=>{if(++attempts>=50)process.exit(1);setTimeout(ping,100)})})()',
            ],
          },
        }
      )
      expect(ready.status()).toBe(200)
      const readyBody = (await ready.json()) as { exit_code: number }
      expect(readyBody.exit_code).toBe(0)

      const minted = await page.request.post(
        `/api/v1/sandboxes/${sandboxId}/preview-link`,
        {
          data: {
            port: 3000,
            path: '/browser-proof?ok=1',
            ttl_seconds: 300,
          },
        }
      )
      expect(minted.status()).toBe(200)
      const { url } = (await minted.json()) as { url: string }
      const shareUrl = new URL(url)
      expect(shareUrl.searchParams.get('session_grant')).toBeNull()
      expect(shareUrl.hash).toMatch(/^#session_grant=./)

      // CI binds preview TLS to 3443. Local slots can exercise the same flow
      // over their HTTP proxy by setting E2E_PREVIEW_SCHEME/E2E_PREVIEW_PORT.
      shareUrl.protocol = process.env.E2E_PREVIEW_SCHEME ?? 'https:'
      shareUrl.port = process.env.E2E_PREVIEW_PORT ?? '3443'

      const networkUrls: string[] = []
      let bridgePosts = 0
      page.on('request', (request) => {
        networkUrls.push(request.url())
        if (
          request.method() === 'POST' &&
          new URL(request.url()).pathname === '/__temps/preview/login'
        ) {
          bridgePosts += 1
        }
      })

      await navigateToNewPreviewRoute(
        page,
        shareUrl.toString(),
        () => bridgePosts
      )
      await page.waitForURL(/\/browser-proof\?ok=1$/)

      expect(bridgePosts).toBe(1)
      expect(
        networkUrls.some((requestUrl) => requestUrl.includes('session_grant=')),
        'the bearer grant must never appear in an HTTP request URL'
      ).toBe(false)
      expect(page.url()).not.toContain('#')
      expect(new URL(page.url()).pathname).toBe('/browser-proof')

      const cookies = await page.context().cookies()
      const previewCookie = cookies.find(
        (cookie) => cookie.name === `temps_preview_${sandboxId}`
      )
      expect(previewCookie).toBeDefined()
      expect(previewCookie?.httpOnly).toBe(true)
      await expect(page.locator('body')).toHaveText(
        'preview-share-ok:/browser-proof?ok=1'
      )
    } finally {
      const destroyed = await page.request.post(
        `/api/v1/sandboxes/${sandboxId}/destroy`
      )
      expect([200, 204]).toContain(destroyed.status())
    }
  })
})
