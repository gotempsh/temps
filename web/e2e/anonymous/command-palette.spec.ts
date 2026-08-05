import { expect, expectAppMounted, test } from '../fixtures'

test.describe('command palette', () => {
  test.beforeEach(async ({ page }) => {
    await page.route('**/api/user/me', async (route) => {
      await route.fulfill({
        contentType: 'application/json',
        json: {
          avatar_url: '',
          email: 'verify@temps.sh',
          id: 999,
          mfa_enabled: false,
          name: 'Verification User',
          role: 'admin',
          username: 'verify',
        },
      })
    })

    await page.route('**/api/projects?*', async (route) => {
      await route.fulfill({
        contentType: 'application/json',
        json: {
          page: 1,
          per_page: 100,
          projects: [
            {
              id: 999,
              name: 'Monitoring App',
              slug: 'monitoring-app',
            },
          ],
          total: 1,
        },
      })
    })

    await page.goto('/settings')
    await expectAppMounted(page)
    await page.getByRole('button', { name: /Find/ }).click()
    await expect(
      page.getByPlaceholder('Type a command or search...')
    ).toBeVisible()
  })

  test('renders a left icon for actions', async ({ page }) => {
    const initialHeadings = page.locator('[cmdk-group-heading]')
    await expect(initialHeadings.first()).toHaveText('Navigation')

    const missingLeftIcons = await page
      .locator('[cmdk-item]')
      .evaluateAll((items) =>
        items
          .filter((item) => {
            const firstChild = item.firstElementChild
            if (!firstChild) return true
            return !(
              firstChild.tagName.toLowerCase() === 'svg' ||
              (firstChild.tagName.toLowerCase() === 'span' &&
                firstChild.classList.contains('size-6'))
            )
          })
          .map((item) => item.textContent?.trim())
      )
    expect(missingLeftIcons).toEqual([])

    await page.getByPlaceholder('Type a command or search...').fill('toggle')

    const themeAction = page.locator('[cmdk-item]').filter({
      hasText: 'Toggle Theme',
    })
    await expect(themeAction).toBeVisible()
    await expect(themeAction.locator(':scope > svg')).toHaveCount(1)
  })

  test('prioritizes project matches over common pages', async ({ page }) => {
    await page.getByPlaceholder('Type a command or search...').fill('monitor')

    const headings = page.locator('[cmdk-group-heading]')
    await expect(headings.first()).toHaveText('Projects')
    await expect(headings.filter({ hasText: 'Navigation' })).toHaveCount(1)

    const project = page.locator('[cmdk-item]').filter({
      hasText: 'monitoring-app',
    })
    await expect(project).toBeVisible()
    await expect(project.locator(':scope > span').first()).toBeVisible()

    const navigationGroup = page.locator('[cmdk-group]').filter({
      has: page.locator('[cmdk-group-heading]', { hasText: 'Navigation' }),
    })
    const navigation = navigationGroup.locator('[cmdk-item]').filter({
      hasText: 'Monitoring',
    })
    await expect(navigation.locator(':scope > svg')).toHaveCount(1)
  })
})
