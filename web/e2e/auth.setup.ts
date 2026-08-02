import { expect, test as setup } from '@playwright/test'
import { ADMIN_EMAIL, ADMIN_PASSWORD, login, URL_PROJECTS } from './fixtures'

const STORAGE_STATE = 'e2e/.auth/user.json'

/**
 * Logs in once and persists cookies + localStorage for every other project.
 *
 * This also serves as the canonical proof that authentication works, so if it
 * fails, every dependent spec is skipped with an obvious cause rather than 12
 * confusing downstream failures.
 */
setup('authenticate', async ({ page }) => {
  await login(page, ADMIN_EMAIL, ADMIN_PASSWORD)

  // Landing on /projects is the signal that the session cookie was accepted --
  // the login form stays put and shows an error otherwise.
  await page.waitForURL(URL_PROJECTS, { timeout: 30_000 })
  await expect(
    page.getByRole('heading', { name: 'Projects', exact: true })
  ).toBeVisible()

  // The first-project tour renders a modal overlay that intercepts pointer
  // events, which would make every click in the project specs time out on a
  // fresh instance. Marking it seen here (it is persisted into the storage
  // state) is the same thing a returning user's browser would have.
  // Key mirrors SEEN_KEY in src/components/project/ProjectTour.tsx.
  await page.evaluate(() =>
    localStorage.setItem('temps.project-tour.v1', 'true')
  )

  await page.context().storageState({ path: STORAGE_STATE })
})
