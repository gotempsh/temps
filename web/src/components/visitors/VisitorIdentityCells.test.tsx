// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, mock, test } from 'bun:test'
import { cleanup, fireEvent, render, screen } from '@testing-library/react'
import type { VisitorInfo } from '@/api/client/types.gen'
import { TooltipProvider } from '@/components/ui/tooltip'
import { installBrowserWindow } from '@/test/dom'
import {
  VisitorLocationCell,
  VisitorSourceCell,
  VisitorTechnologyCell,
} from './VisitorIdentityCells'

installBrowserWindow()
afterEach(cleanup)

function visitor(overrides: Partial<VisitorInfo> = {}): VisitorInfo {
  return {
    id: 1,
    visitor_id: 'seed-v1',
    project_id: 1,
    environment_id: 1,
    first_seen: '2026-11-20T10:00:00Z',
    last_seen: '2026-11-22T10:00:00Z',
    is_crawler: false,
    ...overrides,
  } as VisitorInfo
}

function renderCell(node: React.ReactNode) {
  return render(<TooltipProvider>{node}</TooltipProvider>)
}

test('the location cell is one line: flag, then city and country as filters', () => {
  const onFilter = mock(() => {})
  renderCell(
    <VisitorLocationCell
      visitor={visitor({
        city: 'Toronto',
        region: 'Ontario',
        country: 'Canada',
        country_code: 'CA',
      })}
      activeFilter={(key) => (key === 'filter_country' ? 'Canada' : undefined)}
      onFilter={onFilter}
    />
  )
  expect(screen.getByText('🇨🇦')).toBeTruthy()
  // The region lives in the tooltip, not on the line.
  expect(screen.queryByText('Ontario')).toBeNull()
  fireEvent.click(screen.getByRole('button', { name: 'Filter by Toronto' }))
  expect(onFilter).toHaveBeenLastCalledWith('filter_city', 'Toronto')
  // Clicking the active filter clears it.
  const country = screen.getByRole('button', { name: 'Filter by Canada' })
  expect(country.getAttribute('aria-pressed')).toBe('true')
  fireEvent.click(country)
  expect(onFilter).toHaveBeenLastCalledWith('filter_country', undefined)
})

test('a location with nothing known reads Unknown with a map pin and no filters', () => {
  const { container } = renderCell(
    <VisitorLocationCell
      visitor={visitor()}
      activeFilter={() => undefined}
      onFilter={() => {}}
    />
  )
  expect(screen.getByText('Unknown')).toBeTruthy()
  expect(screen.queryByRole('button')).toBeNull()
  expect(container.querySelector('svg.lucide-map-pin')).toBeTruthy()
})

test('the source cell shows the referrer favicon, the channel and the referrer as filters', () => {
  const onFilter = mock(() => {})
  const { container } = renderCell(
    <VisitorSourceCell
      visitor={visitor({
        first_channel: 'Referral',
        first_referrer: 'https://blog.example.org/posts/launch-week',
        first_referrer_hostname: 'blog.example.org',
      })}
      activeFilter={() => undefined}
      onFilter={onFilter}
    />
  )
  expect(
    container.querySelector('img[alt="blog.example.org favicon"]')
  ).toBeTruthy()
  fireEvent.click(screen.getByRole('button', { name: 'Filter by Referral' }))
  expect(onFilter).toHaveBeenLastCalledWith('filter_channel', 'Referral')
  fireEvent.click(
    screen.getByRole('button', { name: 'Filter by blog.example.org' })
  )
  expect(onFilter).toHaveBeenLastCalledWith(
    'filter_referrer',
    'blog.example.org'
  )
})

test('without a referrer the source cell uses the channel icon; with nothing it reads Direct', () => {
  const { container, unmount } = renderCell(
    <VisitorSourceCell
      visitor={visitor({ first_channel: 'Email' })}
      activeFilter={() => undefined}
      onFilter={() => {}}
    />
  )
  expect(container.querySelector('img')).toBeNull()
  expect(container.querySelector('svg.lucide-mail')).toBeTruthy()
  unmount()
  renderCell(
    <VisitorSourceCell
      visitor={visitor()}
      activeFilter={() => undefined}
      onFilter={() => {}}
    />
  )
  expect(screen.getByText('Direct')).toBeTruthy()
  expect(screen.queryByRole('button')).toBeNull()
})

test('the browser cell shows browser and OS with their icons, and Unknown when neither is known', () => {
  const { container, unmount } = renderCell(
    <VisitorTechnologyCell
      visitor={visitor({
        browser: 'Firefox',
        browser_version: '133.0',
        operating_system: 'Linux',
      })}
    />
  )
  expect(screen.getByText('Firefox')).toBeTruthy()
  expect(screen.getByText('Linux')).toBeTruthy()
  // The version is in the tooltip, not on the line.
  expect(screen.queryByText(/133\.0/)).toBeNull()
  expect(container.querySelector('img[alt="Firefox"]')).toBeTruthy()
  unmount()
  renderCell(
    <VisitorTechnologyCell
      visitor={visitor({ user_agent: 'Mozilla/5.0 (Unparsed)' })}
    />
  )
  expect(screen.getByText('Unknown')).toBeTruthy()
})

test('a crawler, which names no browser, reads Crawler instead of Unknown', () => {
  const { container } = renderCell(
    <VisitorTechnologyCell
      visitor={visitor({
        is_crawler: true,
        crawler_name: 'ExampleBot',
        user_agent: 'ExampleBot/1.0',
      })}
    />
  )
  expect(screen.getByText('Crawler')).toBeTruthy()
  expect(screen.queryByText('Unknown')).toBeNull()
  expect(container.querySelector('svg.lucide-bot')).toBeTruthy()
})

test('every part of a long location and source truncates on its own text', () => {
  renderCell(
    <>
      <VisitorLocationCell
        visitor={visitor({
          city: 'Santa Cruz de la Sierra',
          country: 'Bolivia',
          country_code: 'BO',
        })}
        activeFilter={() => undefined}
        onFilter={() => {}}
      />
      <VisitorSourceCell
        visitor={visitor({
          first_channel: 'Paid Search',
          first_referrer_hostname: 'ads.campaigns.example.com',
        })}
        activeFilter={() => undefined}
        onFilter={() => {}}
      />
    </>
  )
  const city = screen.getByRole('button', {
    name: 'Filter by Santa Cruz de la Sierra',
  })
  const country = screen.getByRole('button', { name: 'Filter by Bolivia' })
  expect(city.className).toContain('truncate')
  // The country yields last, but can still truncate (no `shrink-0`).
  expect(country.className).toContain('shrink-[0.001]')
  expect(country.className).toContain('truncate')
  expect(country.className).not.toContain('shrink-0')
  const referrer = screen.getByRole('button', {
    name: 'Filter by ads.campaigns.example.com',
  })
  expect(referrer.className).toContain('truncate')
  expect(referrer.className).not.toContain('shrink-0')
  expect(
    screen.getByRole('button', { name: 'Filter by Paid Search' }).className
  ).toContain('shrink-[0.001]')
})

test('a line with tooltip details is a keyboard tab stop; one without is not', () => {
  const { container, unmount } = renderCell(
    <VisitorTechnologyCell
      visitor={visitor({
        browser: 'Chrome',
        browser_version: '131.0.0.0',
        operating_system: 'Windows 10',
      })}
    />
  )
  const line = container.querySelector('[tabindex="0"]')
  expect(line?.textContent).toContain('Chrome')
  unmount()
  const bare = renderCell(<VisitorTechnologyCell visitor={visitor()} />)
  expect(bare.container.querySelector('[tabindex]')).toBeNull()
})
