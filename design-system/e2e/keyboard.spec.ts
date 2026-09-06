// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test, type Page } from '@playwright/test'
import { ready, v1 } from './helpers'

/**
 * The keyboard contract from docs/design-system-handoff.md §9.
 *
 * The rule the whole file exists for is "the cursor is the focus": `j`, `k`
 * and the arrows do not paint a bar, they move DOM focus onto the row they
 * mark, so `⏎` always acts on the row the reader can see is current. A test
 * that only checked `aria-current` would pass on the exact bug §9 warns about.
 */

/** What the browser thinks is focused, in ledger terms. */
async function focused(page: Page) {
  return page.evaluate(() => {
    const el = document.activeElement as HTMLElement | null
    return {
      tag: el?.tagName ?? null,
      isRow: !!el?.classList.contains('op-row'),
      id: el?.id ?? null,
      ariaCurrent: el?.getAttribute('aria-current') ?? null,
      tabIndex: el?.tabIndex ?? null,
    }
  })
}

/** The id of the one row the ledger marks as current. */
async function cursorRowId(page: Page) {
  return page.evaluate(() => document.querySelector('.op-row[aria-current]')?.id ?? null)
}

/**
 * Open a Detail facet by its tab label. The accessible name carries the tab's
 * number badge too ("events 5"), so the match is a prefix, not an equality.
 */
function tabByLabel(page: Page, label: string) {
  return page.getByRole('tab').filter({ has: page.locator(`text="${label}"`) }).first()
}

async function openTab(page: Page, label: string) {
  const tab = tabByLabel(page, label)
  await tab.click()
  await expect(tab).toHaveAttribute('aria-selected', 'true')
}

test.describe('ledger keyboard', () => {
  test('j and k move DOM focus onto the marked row', async ({ page }) => {
    await page.goto(v1('analytics'))
    await ready(page)
    await openTab(page, 'events')

    const rows = page.locator('.op-rows .op-row[tabindex]')
    await expect(rows.first()).toBeVisible()
    const count = await rows.count()
    expect(count).toBeGreaterThan(2)

    // The cursor starts on row 0 and nothing is focused yet: pressing j must
    // both move the mark and take the focus with it.
    await page.locator('body').click({ position: { x: 5, y: 5 } })
    await page.keyboard.press('j')

    let state = await focused(page)
    expect(state.isRow, 'j must move focus onto a row, not merely paint a bar').toBe(true)
    expect(state.ariaCurrent).toBe('true')
    expect(state.tabIndex).toBe(0)
    expect(await cursorRowId(page)).toBe(state.id)
    const second = state.id

    await page.keyboard.press('j')
    state = await focused(page)
    expect(state.isRow).toBe(true)
    expect(state.id).not.toBe(second)
    const third = state.id

    await page.keyboard.press('k')
    state = await focused(page)
    expect(state.id, 'k moves back up').toBe(second)
    expect(state.id).not.toBe(third)
    expect(await cursorRowId(page)).toBe(second)
  })

  test('arrow keys do what j and k do', async ({ page }) => {
    await page.goto(v1('analytics'))
    await ready(page)
    await openTab(page, 'events')
    await page.locator('body').click({ position: { x: 5, y: 5 } })

    await page.keyboard.press('ArrowDown')
    const down = await focused(page)
    expect(down.isRow).toBe(true)
    expect(down.ariaCurrent).toBe('true')

    await page.keyboard.press('ArrowUp')
    const up = await focused(page)
    expect(up.isRow).toBe(true)
    expect(up.id).not.toBe(down.id)
  })

  test('Enter opens the record the cursor marks', async ({ page }) => {
    await page.goto(v1('analytics'))
    await ready(page)
    await openTab(page, 'events')
    await page.locator('body').click({ position: { x: 5, y: 5 } })

    await page.keyboard.press('j')
    expect((await focused(page)).isRow).toBe(true)
    await page.keyboard.press('Enter')

    await expect(page).toHaveURL(/p=event%3A/)
  })

  test('Tab from the filter lands on the row the cursor marks', async ({ page }) => {
    await page.goto(v1('analytics'))
    await ready(page)
    await openTab(page, 'events')

    const filter = page.getByRole('textbox', { name: 'filter events' })
    await filter.focus()
    await expect(filter).toBeFocused()

    // Roving tabindex: exactly one row is in the tab order, and it is the
    // marked one. Everything between the filter and the list is skipped over
    // in at most a handful of stops.
    let state = await focused(page)
    for (let i = 0; i < 8 && !state.isRow; i++) {
      await page.keyboard.press('Tab')
      state = await focused(page)
    }
    expect(state.isRow, 'tabbing forward from the filter must reach a row').toBe(true)
    expect(state.ariaCurrent).toBe('true')
    expect(await cursorRowId(page)).toBe(state.id)

    // and only one row is tabbable
    const tabbable = await page.locator('.op-rows .op-row[tabindex="0"]').count()
    expect(tabbable).toBe(1)
  })

  test('slash focuses the filter', async ({ page }) => {
    await page.goto(v1('analytics'))
    await ready(page)
    await openTab(page, 'events')
    await page.locator('body').click({ position: { x: 5, y: 5 } })

    const filter = page.getByRole('textbox', { name: 'filter events' })
    await expect(filter).not.toBeFocused()
    await page.keyboard.press('/')
    await expect(filter).toBeFocused()
    // `/` focuses, it does not type itself into the box.
    await expect(filter).toHaveValue('')
  })

  test('keys are ignored while an input has focus', async ({ page }) => {
    await page.goto(v1('analytics'))
    await ready(page)
    await openTab(page, 'events')

    const filter = page.getByRole('textbox', { name: 'filter events' })
    await filter.focus()

    // j, k and 1 are all bound keys; inside the box they are just characters.
    await page.keyboard.type('jk1')
    await expect(filter).toBeFocused()
    await expect(filter).toHaveValue('jk1')
    // Focus never left the box for a row, and the Detail did not switch facets
    // on the "1". (The list narrowing to nothing is the filter doing its job.)
    expect((await focused(page)).tag).toBe('INPUT')
    expect((await focused(page)).isRow).toBe(false)
    await expect(tabByLabel(page, 'events')).toHaveAttribute('aria-selected', 'true')

    // Enter in the filter does not open the cursor row either: the window
    // handler only opens on ⏎ when nothing is focused (§9).
    const url = page.url()
    await filter.fill('')
    await page.keyboard.press('Enter')
    expect((await focused(page)).tag).toBe('INPUT')
    expect(page.url()).toBe(url)
  })
})

test.describe('pager keyboard', () => {
  test('] pages the issue events ledger and the pager text changes', async ({ page }) => {
    await page.goto(v1('issue:i_4821'))
    await ready(page)
    await openTab(page, 'events')

    const pager = page.locator('.op-rows').locator('text=/\\d+–\\d+ of/').first()
    await expect(pager).toBeVisible()
    const first = (await pager.textContent())?.trim()
    expect(first).toMatch(/^1–/)

    await page.keyboard.press(']')
    await expect(pager).not.toHaveText(first!)
    const second = (await pager.textContent())?.trim()
    expect(second).not.toBe(first)

    await page.keyboard.press('[')
    await expect(pager).toHaveText(first!)
  })
})

test.describe('detail keyboard', () => {
  test('digits switch facets', async ({ page }) => {
    await page.goto(v1('issue:i_4821'))
    await ready(page)

    const tabs = page.getByRole('tab')
    const labels = await tabs.allInnerTexts()
    expect(labels.length).toBeGreaterThan(1)

    await page.locator('body').click({ position: { x: 5, y: 5 } })
    for (let i = 0; i < labels.length; i++) {
      await page.keyboard.press(String(i + 1))
      await expect(tabs.nth(i)).toHaveAttribute('aria-selected', 'true')
    }
    // back to the first facet
    await page.keyboard.press('1')
    await expect(tabs.nth(0)).toHaveAttribute('aria-selected', 'true')
  })

  test('digits are ignored while a filter has focus', async ({ page }) => {
    await page.goto(v1('issue:i_4821'))
    await ready(page)
    await openTab(page, 'events')

    const filter = page.getByRole('textbox', { name: 'filter by event id' })
    await filter.focus()
    await page.keyboard.type('3')
    await expect(filter).toHaveValue('3')
    await expect(tabByLabel(page, 'events')).toHaveAttribute('aria-selected', 'true')
  })
})
