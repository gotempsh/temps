// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, describe, expect, test } from 'bun:test'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
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

const CREDENTIALS_SCHEMA = {
  type: 'object',
  required: ['database'],
  properties: {
    database: { type: 'string', default: 'postgres' },
    username: { type: 'string', default: 'postgres' },
    max_connections: { type: 'string', default: '100' },
  },
}

describe('JsonSchemaForm with every field advanced', () => {
  afterEach(cleanup)

  test('asks for nothing up front and submits the defaults', async () => {
    const submitted: Record<string, unknown>[] = []
    const view = render(
      <JsonSchemaForm
        schema={CREDENTIALS_SCHEMA}
        onSubmit={(values) => {
          submitted.push(values)
        }}
        serviceType="postgres"
        managedByTemps
        allFieldsAdvanced
        advancedLabel="Advanced settings"
        advancedContent={<p>Deployment node</p>}
        submitText="Create"
      />
    )
    expect(view.queryByLabelText(/Database/)).toBeNull()
    expect(view.queryByText('Deployment node')).toBeNull()

    fireEvent.click(view.getByRole('button', { name: /Advanced settings/ }))
    expect(view.getByText('Deployment node')).toBeTruthy()
    expect(view.getByLabelText(/Database/)).toBeTruthy()
    expect(view.getByLabelText(/Max Connections/)).toBeTruthy()

    fireEvent.click(view.getByRole('button', { name: /Advanced settings/ }))
    fireEvent.click(view.getByRole('button', { name: 'Create' }))
    await waitFor(() => expect(submitted).toHaveLength(1))
    expect(submitted[0]).toMatchObject({
      database: 'postgres',
      username: 'postgres',
    })
  })

  test('opens the section when a field in it fails validation', async () => {
    const submitted: unknown[] = []
    const view = render(
      <JsonSchemaForm
        schema={{
          ...CREDENTIALS_SCHEMA,
          properties: {
            ...CREDENTIALS_SCHEMA.properties,
            database: { type: 'string' },
          },
        }}
        onSubmit={(values) => {
          submitted.push(values)
        }}
        serviceType="postgres"
        allFieldsAdvanced
        advancedLabel="Advanced settings"
        submitText="Create"
      />
    )
    expect(view.queryByLabelText(/Database/)).toBeNull()
    fireEvent.click(view.getByRole('button', { name: 'Create' }))
    await waitFor(() => expect(view.getByLabelText(/Database/)).toBeTruthy())
    await waitFor(() =>
      expect(view.getByText('Database is required')).toBeTruthy()
    )
    expect(submitted).toHaveLength(0)
  })
})
