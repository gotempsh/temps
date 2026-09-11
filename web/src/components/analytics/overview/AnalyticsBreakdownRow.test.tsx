import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { AnalyticsBreakdownRow } from './AnalyticsBreakdownRow'
import {
  countryFlag,
  dimensionLabel,
  AnalyticsDimensionIcon,
} from './AnalyticsDimensionIdentity'

test('country codes and names resolve to the same flag', () => {
  expect(countryFlag('Singapore')).toBe('🇸🇬')
  expect(countryFlag('US')).toBe('🇺🇸')
  expect(countryFlag('United States')).toBe('🇺🇸')
  expect(countryFlag('Unknown')).toBeUndefined()
})
test('global breakdowns use readable language and referrer labels', () => {
  expect(dimensionLabel('language', 'en-US')).toBe('English (US)')
  expect(dimensionLabel('referrer_hostname', 'www.google.com')).toBe('Google')
  expect(dimensionLabel('referrer_hostname', '')).toBe('Direct')
  expect(dimensionLabel('country', 'SG')).toBe('Singapore')
})
test('ranked rows render logos, percentages, visitor counts and matching bars', () => {
  const html = renderToStaticMarkup(
    <AnalyticsBreakdownRow
      label="Google"
      icon={
        <AnalyticsDimensionIcon
          dimension="referrer_hostname"
          value="google.com"
        />
      }
      count={89}
      percentage={18.6}
    />
  )
  expect(html).toContain('google.com')
  expect(html).toContain('18.6%')
  expect(html).toContain('89')
  expect(html).toContain('width:18.6%')
  expect(html).not.toContain('<button')
})
test('zero or invalid totals never produce NaN bar widths', () => {
  const html = renderToStaticMarkup(
    <AnalyticsBreakdownRow
      label="Unknown"
      icon={null}
      count={0}
      percentage={NaN}
    />
  )
  expect(html).toContain('width:0%')
  expect(html).not.toContain('NaN')
})
