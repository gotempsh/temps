// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from '@playwright/test'
import {
  RUNTIME_LOG_ROW_CLASS,
  RUNTIME_LOG_METADATA_CLASS,
  RUNTIME_LOG_MESSAGE_CLASS,
} from '../../src/components/runtime-logs/runtime-log-row-layout'

function renderRow(metadata: string, message: string): string {
  return `<div class="${RUNTIME_LOG_ROW_CLASS}"><div class="${RUNTIME_LOG_METADATA_CLASS}">${metadata}</div><span class="${RUNTIME_LOG_MESSAGE_CLASS}">${message}</span></div>`
}

test('runtime log messages use the phone width without overflowing', async ({
  page,
}, testInfo) => {
  const message = `INFO relay connection started route_id=${'a'.repeat(72)} connection_id=${'b'.repeat(72)}`
  const live = renderRow(
    '<span class="shrink-0 tabular-nums sm:w-[85px]">08:09:03.603</span><span class="shrink-0">INFO</span><span class="min-w-0 max-w-full truncate sm:w-[120px] sm:shrink-0">example-relay-service</span>',
    message
  )
  const history = renderRow(
    '<span class="shrink-0 tabular-nums sm:w-[180px]">Wed Sep 23 08:09:03.603</span><span class="shrink-0">INFO</span><span class="min-w-0 max-w-full truncate sm:w-[70px] sm:shrink-0">example-relay-service</span><span class="min-w-0 max-w-full truncate sm:w-[150px] sm:shrink-0">example-node · example-container</span>',
    message
  )

  await page.setViewportSize({ width: 375, height: 812 })
  await page.goto('/login')
  await page.evaluate(
    ({ live, history }) => {
      const fixture = document.createElement('main')
      fixture.id = 'log-layout-fixture'
      fixture.style.cssText =
        'position:fixed;top:0;left:50%;transform:translateX(-50%);width:340px;height:100vh;z-index:2147483647;background:var(--background)'
      fixture.innerHTML = `<section id="live">${live}</section><section id="history">${history}</section>`
      document.body.append(fixture)
    },
    { live, history }
  )
  await expect
    .poll(() =>
      page
        .locator('#live > div')
        .evaluate((row) => getComputedStyle(row).flexDirection)
    )
    .toBe('column')

  for (const id of ['live', 'history']) {
    const row = page.locator(`#${id} > div`)
    const metadata = row.locator('div').first()
    const text = row.locator('span').last()
    const rowBox = await row.boundingBox()
    const metadataBox = await metadata.boundingBox()
    const textBox = await text.boundingBox()
    expect(rowBox).not.toBeNull()
    expect(metadataBox).not.toBeNull()
    expect(textBox).not.toBeNull()
    expect(textBox!.y).toBeGreaterThanOrEqual(
      metadataBox!.y + metadataBox!.height
    )
    expect(textBox!.width).toBeGreaterThan(rowBox!.width * 0.85)
    expect(
      await row.evaluate(
        (element) => element.scrollWidth <= element.clientWidth
      )
    ).toBe(true)
  }
  await page.screenshot({
    path: testInfo.outputPath('runtime-logs-mobile.png'),
  })

  await page.setViewportSize({ width: 1280, height: 812 })
  await page.locator('#log-layout-fixture').evaluate((element) => {
    element.style.width = '1100px'
  })
  for (const id of ['live', 'history']) {
    const row = page.locator(`#${id} > div`)
    const timestamp = row.locator('span').first()
    const text = row.locator('span').last()
    const timestampBox = await timestamp.boundingBox()
    const textBox = await text.boundingBox()
    expect(textBox!.x).toBeGreaterThan(timestampBox!.x)
    expect(Math.abs(textBox!.y - timestampBox!.y)).toBeLessThan(5)
  }
})
