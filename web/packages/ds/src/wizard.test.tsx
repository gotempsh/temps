// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { Wizard } from './wizard'

const steps = [
  { id: 'provider', label: 'Choose provider', description: 'Where code lives' },
  { id: 'repository', label: 'Select repository' },
  { id: 'done', label: 'Connection ready' },
]

describe('Wizard', () => {
  test('uses one shared heading and distinguishes current and completed steps', () => {
    const markup = renderToStaticMarkup(
      <Wizard
        title="Connect repository"
        description="Choose a source"
        steps={steps}
        currentStep="repository"
        headerActions={<button>Help</button>}
      >
        <label>
          Repository
          <input name="repository" />
        </label>
      </Wizard>
    )
    expect(markup.match(/<h1/g)).toHaveLength(1)
    expect(markup).toContain('data-page-header')
    expect(markup).toContain('aria-label="Choose provider, completed"')
    expect(markup).toContain(
      'aria-current="step" aria-label="Select repository"'
    )
    expect(markup).toContain('Where code lives')
    expect(markup).toContain('Help')
    expect(markup).toContain('name="repository"')
  })

  test('renders an optional action footer after the form without nesting it inside the form', () => {
    const markup = renderToStaticMarkup(
      <Wizard
        title="Connect repository"
        description="Choose a source"
        steps={steps}
        currentStep="provider"
        footer={
          <button type="submit" form="setup">
            Continue
          </button>
        }
      >
        <form id="setup">
          <input name="repository" />
        </form>
      </Wizard>
    )
    expect(markup).toContain('<section')
    expect(markup.indexOf('</form>')).toBeLessThan(
      markup.indexOf('form="setup"')
    )
  })
})
