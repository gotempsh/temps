import { test, expect, describe } from 'bun:test'
import { selectIntegration } from './index.js'

const stripe = { id: 1, provider: 'stripe', status: 'active', webhook_path: '/wh/1', last_event_at: null }
const paddle = { id: 2, provider: 'paddle', status: 'active', webhook_path: '/wh/2', last_event_at: null }
const stripeTest = { id: 3, provider: 'stripe', status: 'active', webhook_path: '/wh/3', last_event_at: null }

describe('selectIntegration', () => {
  test('picks the integration by --integration-id', () => {
    expect(selectIntegration([stripe, paddle], { integrationId: '2' })).toBe(paddle)
  })

  test('rejects a non-numeric --integration-id', () => {
    expect(() => selectIntegration([stripe], { integrationId: 'abc' })).toThrow(
      /Invalid --integration-id/,
    )
  })

  test('rejects a zero or negative --integration-id', () => {
    expect(() => selectIntegration([stripe], { integrationId: '0' })).toThrow(
      /Invalid --integration-id/,
    )
    expect(() => selectIntegration([stripe], { integrationId: '-1' })).toThrow(
      /Invalid --integration-id/,
    )
  })

  test('reports when --integration-id matches nothing in this project', () => {
    expect(() => selectIntegration([stripe], { integrationId: '99' })).toThrow(
      /Integration 99 not found/,
    )
  })

  test('errors out when the project has no integrations at all', () => {
    expect(() => selectIntegration([], {})).toThrow(/No revenue integrations configured/)
  })

  test('picks the single integration matching --provider', () => {
    expect(selectIntegration([stripe, paddle], { provider: 'paddle' })).toBe(paddle)
  })

  test('rejects a --provider with no matches, listing what is available', () => {
    expect(() => selectIntegration([stripe, paddle], { provider: 'braintree' })).toThrow(
      /No integration with provider "braintree"\. Available: stripe, paddle/,
    )
  })

  test('demands --integration-id when --provider matches more than one integration', () => {
    expect(() => selectIntegration([stripe, stripeTest], { provider: 'stripe' })).toThrow(
      /Multiple integrations for provider "stripe"/,
    )
  })

  test('auto-selects the only integration when no flags were given', () => {
    expect(selectIntegration([stripe], {})).toBe(stripe)
  })

  test('demands --provider or --integration-id when multiple integrations exist and neither flag was given', () => {
    // Silently picking one here would import data into the wrong provider's
    // integration — force disambiguation instead.
    expect(() => selectIntegration([stripe, paddle], {})).toThrow(
      /Multiple integrations configured \(stripe \[id=1\], paddle \[id=2\]\)/,
    )
  })
})
