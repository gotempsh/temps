// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, expectAppMounted, test } from '../fixtures'

/**
 * The full-screen AI chat (`/chat`) has to fit the shell's content slot exactly:
 * its composer is bottom-anchored, so any overflow pushes the textarea below the
 * fold and the user has to scroll the whole page down to type.
 *
 * That is a real regression this guards against -- the page used to size itself
 * with viewport math (`calc(100dvh - 3.5rem)`), which ignored the shell's own
 * padding, the true 4rem header, and the app-wide banners (disk space, update
 * available). With a banner up it overflowed by ~90px and the composer was cut
 * in half.
 *
 * The assertion is on the content slot rather than on a screenshot because this
 * is a layout invariant, not an appearance one: the page must never overflow
 * the shell's content area, at any viewport, with or without banners. `/chat`
 * is a focus mode whose slot clips (`overflow: hidden`) instead of scrolling,
 * so an overflow there would not scroll at all -- it would cut the composer
 * off -- which is why the slot's own bounds are checked against the viewport
 * too.
 */
test.describe('AI chat layout', () => {
  test('fits the content area without making the shell scroll', async ({
    page,
    consoleErrors,
  }) => {
    await page.goto('/chat')
    await expectAppMounted(page)

    // The chat list renders after the conversations request settles; measuring
    // before that can catch an intermediate layout and pass by accident.
    await expect(
      page.getByRole('heading', { name: 'Chats', exact: true })
    ).toBeVisible()

    const overflow = await page.evaluate(() => {
      // The shell's page-content slot: the only box between <main> and the
      // routed page that bounds it. It scrolls on console pages and clips in
      // focus mode; either way, content taller than it overflowed.
      const slot = Array.from(document.querySelectorAll('main > div')).find(
        (element) =>
          ['auto', 'hidden'].includes(getComputedStyle(element).overflowY)
      ) as HTMLElement | undefined

      if (!slot) return null
      return {
        scrollHeight: slot.scrollHeight,
        clientHeight: slot.clientHeight,
        bottom: slot.getBoundingClientRect().bottom,
        viewportHeight: window.innerHeight,
      }
    })

    expect(overflow, 'the shell content slot should be findable').not.toBeNull()
    expect(
      overflow!.scrollHeight,
      'the AI chat page must not overflow the shell content area -- when it does, the bottom-anchored composer falls below the fold'
    ).toBeLessThanOrEqual(overflow!.clientHeight)
    expect(
      overflow!.bottom,
      'the content slot must end inside the viewport, or its bottom rows are off screen'
    ).toBeLessThanOrEqual(overflow!.viewportHeight)

    expect(consoleErrors).toEqual([])
  })
})
