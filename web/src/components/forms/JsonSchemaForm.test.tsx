// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'

import { JsonSchemaForm } from './JsonSchemaForm'

const SCHEMA = {
  type: 'object',
  properties: {
    docker_image: { type: 'string', description: 'Docker image to run' },
  },
}

describe('JsonSchemaForm', () => {
  test('submitDisabled disables the submit button without showing the submitting spinner', () => {
    const markup = renderToStaticMarkup(
      <JsonSchemaForm
        schema={SCHEMA}
        onSubmit={() => {}}
        submitText="Create my-service"
        submitDisabled
      />
    )

    expect(markup).toContain('Create my-service')
    expect(markup).not.toContain('Submitting...')
    // The submit button is the only <button type="submit"> JsonSchemaForm renders.
    const submitButtonMarkup = markup.slice(markup.indexOf('type="submit"'))
    expect(submitButtonMarkup.slice(0, 60)).toContain('disabled=""')
  })

  test('submit button is enabled by default', () => {
    const markup = renderToStaticMarkup(
      <JsonSchemaForm
        schema={SCHEMA}
        onSubmit={() => {}}
        submitText="Create Service"
      />
    )

    const submitButtonMarkup = markup.slice(markup.indexOf('type="submit"'))
    expect(submitButtonMarkup.slice(0, 60)).not.toContain('disabled=""')
  })
})
