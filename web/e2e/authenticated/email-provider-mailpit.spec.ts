// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, expectAppMounted, test, uniqueSlug } from '../fixtures'

/**
 * Drives the console's "Add Email Provider" wizard to create an SMTP
 * provider pointed at a local Mailpit instance, sends a test email through
 * the console's "Send Test Email" dialog, and confirms Mailpit actually
 * received it -- not just that the send call returned success.
 *
 * `POST /email-providers/{id}/test` (what "Send Test Email" calls) sends
 * directly through the provider with no sending-domain verification
 * prerequisite, unlike the general `/emails` send endpoint used by deployed
 * apps -- which is also why this is the one email flow reachable end-to-end
 * from a single console screen. There's no console UI for composing an
 * arbitrary tracked send (open/click pixels), so that path isn't covered
 * here; it's exercised via the API in `apps/temps-e2e`'s `email-scenario`.
 *
 * Requires `docker compose -f apps/temps-e2e/docker-compose.e2e.yml up -d`
 * (Mailpit on localhost:1025 / :8025) and the target instance's outbound SMTP
 * to be able to reach that Mailpit instance.
 */
test.describe('Email provider via Mailpit (SMTP)', () => {
  test('creates an SMTP provider and a test send is actually received by Mailpit', async ({
    page,
  }, testInfo) => {
    const name = uniqueSlug('e2e-mailpit', testInfo)
    let providerId: number | null = null

    try {
      await page.goto('/email/providers/new')
      await expectAppMounted(page)

      // Step 1: provider type. The card is a plain <button> with no
      // aria-label, but its <h3> IS a real heading element -- clicking it
      // bubbles to the ancestor button's onClick like a real user's click
      // would.
      await page.getByRole('heading', { name: 'SMTP', exact: true }).click()

      // Step 2: SMTP configuration, pointed at the local Mailpit catcher.
      await expect(
        page.getByText('Configure SMTP', { exact: false })
      ).toBeVisible()
      // exact: true -- getByLabel does substring matching, and "Username
      // (optional)" contains "name" (case-insensitively), so a plain
      // getByLabel('Name') resolves to two elements.
      await page.getByLabel('Name', { exact: true }).fill(name)
      await page.getByLabel('SMTP Host').fill('localhost')
      await page.getByLabel('Port').fill('1025')
      await page.getByLabel('Encryption').click()
      await page
        .getByRole('option', { name: 'No encryption (local testing only)' })
        .click()
      // Username/password left blank -- Mailpit accepts unauthenticated relays.

      await page.getByRole('button', { name: 'Add Provider' }).click()
      await page.waitForURL(/\/email(\?tab=providers)?$/)

      // Resolve the id via the API (same pattern as api-key-create.spec.ts) --
      // needed both to navigate straight to the provider's detail page and
      // for cleanup.
      const listResponse = await page.request.get('/api/email-providers')
      const providers = (await listResponse.json()) as {
        id: number
        name: string
      }[]
      providerId = providers.find((p) => p.name === name)?.id ?? null
      expect(
        providerId,
        'the created provider should be resolvable by name'
      ).not.toBeNull()

      await page.goto(`/email/providers/${providerId}`)
      await expectAppMounted(page)

      await page.getByRole('button', { name: 'Send Test Email' }).click()
      const dialog = page.getByRole('dialog')
      await expect(
        dialog.getByRole('heading', { name: 'Send Test Email' })
      ).toBeVisible()
      const fromAddress = `noreply@${name}.e2e.test`
      await dialog.getByLabel('From Email Address').fill(fromAddress)
      await dialog.getByRole('button', { name: 'Send Test Email' }).click()

      await expect(page.getByText('Test email sent successfully!')).toBeVisible(
        {
          timeout: 15_000,
        }
      )

      // The real proof: query Mailpit's own REST API for the message. The
      // send-call succeeding is not evidence of delivery -- temps silently
      // "captures" (never actually sends) on provider/config problems while
      // still returning success, which is exactly the gap this test closes.
      await expect(async () => {
        const mailpitRes = await page.request.get(
          'http://localhost:8025/api/v1/messages?limit=50'
        )
        expect(mailpitRes.ok()).toBe(true)
        const data = (await mailpitRes.json()) as {
          messages: { Subject: string; From: { Address: string } }[]
        }
        const received = data.messages.some(
          (m) => m.Subject.includes(name) && m.From.Address === fromAddress
        )
        expect(
          received,
          `expected a message mentioning "${name}" in Mailpit`
        ).toBe(true)
      }).toPass({ timeout: 15_000 })
    } finally {
      if (providerId !== null) {
        await page.request.delete(`/api/email-providers/${providerId}`)
      }
    }
  })
})
