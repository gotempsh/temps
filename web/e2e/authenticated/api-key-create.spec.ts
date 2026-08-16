import { expect, expectAppMounted, test, uniqueSlug } from '../fixtures'

/**
 * Creates an API key through the 3-step console wizard (Basic Info ->
 * Permissions -> Review), entirely through the UI.
 *
 * This is the security-auth subsystem's first end-to-end coverage: until now
 * nothing exercised the RBAC role-assignment path or proved a key minted
 * through the console actually authenticates against the API -- both are
 * exactly the kind of thing that type-checks and builds fine while being
 * silently broken at runtime.
 */
test.describe('API key creation', () => {
  test('creates a key with a predefined role and the secret authenticates', async ({
    page,
  }, testInfo) => {
    const name = uniqueSlug('e2e-key', testInfo)

    await page.goto('/settings/keys/new')
    await expectAppMounted(page)
    await expect(
      page.getByRole('heading', { name: 'Create API Key', exact: true })
    ).toBeVisible()

    // Step 1: Basic Info
    await page.getByLabel('API Key Name *').fill(name)
    await page.getByRole('button', { name: 'Next: Permissions' }).click()

    // Step 2: Permissions -- pick whichever predefined role renders first
    // rather than hardcoding a role name, since the role catalog is backend
    // data, not something this spec should need to know in advance.
    // shadcn's CardTitle renders a <div>, not a heading element, so this is
    // getByText rather than getByRole('heading').
    await expect(
      page.getByText('Choose Access Level', { exact: true })
    ).toBeVisible()
    const firstRoleCard = page
      .locator('div.cursor-pointer', { has: page.locator('.font-medium') })
      .first()
    const roleName = (
      await firstRoleCard.locator('.font-medium').first().textContent()
    )?.trim()
    expect(
      roleName,
      'expected at least one predefined role to be offered'
    ).toBeTruthy()
    await firstRoleCard.click()
    await page.getByRole('button', { name: 'Next: Review' }).click()

    // Step 3: Review -- the chosen role must actually be reflected back.
    await expect(
      page.getByText(roleName!, { exact: false }).first()
    ).toBeVisible()

    let apiKeyId: number | null = null
    await page.getByRole('button', { name: 'Create API Key' }).click()

    // A fresh, non-MFA-enrolled dev admin is not expected to hit step-up
    // verification, but if the backend ever changes that policy this should
    // fail loudly here rather than hang, so assert directly on the outcome.
    await expect(
      page.getByText('API Key Created Successfully', { exact: true })
    ).toBeVisible({ timeout: 15_000 })

    try {
      // The "Your API Key" label isn't programmatically associated with the
      // input (no htmlFor/id), so getByLabel can't resolve it -- it's the
      // one readonly input on this success screen instead.
      const apiKeySecret = await page
        .locator('input[readonly]')
        .first()
        .inputValue()
      expect(
        apiKeySecret.length,
        'the created key secret should be non-empty'
      ).toBeGreaterThan(0)

      // The secret must actually authenticate -- creating a row is not the
      // same as the key working. `whoami`-equivalent: list projects with it.
      const authedResponse = await page.request.get('/api/projects', {
        headers: { Authorization: `Bearer ${apiKeySecret}` },
      })
      expect(
        authedResponse.status(),
        'the newly created key should authenticate'
      ).toBe(200)

      // And it must be listed, not just usable -- proves the create flow
      // persisted, not just returned an optimistic response.
      await page.goto('/settings/keys')
      await expect(page.getByText(name, { exact: true }).first()).toBeVisible({
        timeout: 15_000,
      })

      const listResponse = await page.request.get('/api/api-keys')
      const { api_keys: keys } = (await listResponse.json()) as {
        api_keys: { id: number; name: string }[]
      }
      apiKeyId = keys.find((k) => k.name === name)?.id ?? null
      expect(
        apiKeyId,
        'the created key should be resolvable by name for cleanup'
      ).not.toBeNull()
    } finally {
      if (apiKeyId !== null) {
        await page.request.delete(`/api/api-keys/${apiKeyId}`)
      }
    }
  })
})
