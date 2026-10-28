// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import type { ProjectResponse } from '@/api/client'
import { installBrowserWindow } from '@/test/dom'
import { DockerfileRuntimeSettings } from './DockerfileRuntimeSettings'

installBrowserWindow()
afterEach(cleanup)

const project = {
  id: 7,
  preset: 'dockerfile',
  preset_config: {
    dockerfilePath: 'docker/worker.Dockerfile',
    command: ['python', '-m', 'worker'],
    entrypoint: ['/app/start'],
  },
} as unknown as ProjectResponse

test('saves exact arguments and can restore defaults without losing build settings', async () => {
  const saves: unknown[] = []
  const view = render(
    <DockerfileRuntimeSettings
      project={project}
      saving={false}
      onSave={async (data) => {
        saves.push(data)
      }}
    />
  )
  const command = view.getByLabelText('Command (CMD)')
  const entrypoint = view.getByLabelText('Entrypoint (ENTRYPOINT)')
  fireEvent.change(command, {
    target: { value: '["worker", "argument with spaces", ""]' },
  })
  fireEvent.change(entrypoint, { target: { value: '[]' } })
  fireEvent.click(view.getByRole('button', { name: 'Save startup settings' }))
  await waitFor(() => expect(saves.length).toBe(1))
  expect(saves[0]).toEqual({
    preset_config: {
      dockerfilePath: 'docker/worker.Dockerfile',
      command: ['worker', 'argument with spaces', ''],
      entrypoint: [],
    },
  })
  await waitFor(() => {
    const button = view.getByRole('button', { name: 'Save startup settings' })
    expect(button.getAttribute('aria-busy')).toBe('false')
    expect(button.getAttribute('aria-disabled')).toBe('true')
  })
  fireEvent.change(command, { target: { value: '' } })
  fireEvent.change(entrypoint, { target: { value: '' } })
  await waitFor(() =>
    expect(
      view
        .getByRole('button', { name: 'Save startup settings' })
        .getAttribute('aria-disabled')
    ).toBe('false')
  )
  fireEvent.click(view.getByRole('button', { name: 'Save startup settings' }))
  await waitFor(() => expect(saves.length).toBe(2))
  expect(saves[1]).toEqual({
    preset_config: {
      dockerfilePath: 'docker/worker.Dockerfile',
      command: null,
      entrypoint: null,
    },
  })
})

test('keeps edits on failure and allows retry', async () => {
  let attempts = 0
  const view = render(
    <DockerfileRuntimeSettings
      project={project}
      saving={false}
      onSave={async () => {
        attempts++
        if (attempts === 1) throw new Error('offline')
      }}
    />
  )
  const command = view.getByLabelText('Command (CMD)') as HTMLTextAreaElement
  fireEvent.change(command, { target: { value: '["worker"]' } })
  fireEvent.click(view.getByRole('button', { name: 'Save startup settings' }))
  await view.findByText(/Could not save container startup settings/)
  expect(command.value).toBe('["worker"]')
  fireEvent.click(view.getByRole('button', { name: 'Save startup settings' }))
  await waitFor(() => expect(attempts).toBe(2))
})

test('preserves unsaved edits on refetch and resets when switching projects', () => {
  const onSave = async () => {}
  const view = render(
    <DockerfileRuntimeSettings
      project={project}
      saving={false}
      onSave={onSave}
    />
  )
  fireEvent.change(view.getByLabelText('Command (CMD)'), {
    target: { value: '["my-draft"]' },
  })
  const refreshed = {
    ...project,
    preset_config: { command: ['remote-command'], entrypoint: ['/new-start'] },
  } as unknown as ProjectResponse
  view.rerender(
    <DockerfileRuntimeSettings
      project={refreshed}
      saving={false}
      onSave={onSave}
    />
  )
  expect(
    (view.getByLabelText('Command (CMD)') as HTMLTextAreaElement).value
  ).toBe('["my-draft"]')
  expect(
    (view.getByLabelText('Entrypoint (ENTRYPOINT)') as HTMLTextAreaElement)
      .value
  ).toBe('["/new-start"]')
  view.rerender(
    <DockerfileRuntimeSettings
      project={{ ...refreshed, id: 8 }}
      saving={false}
      onSave={onSave}
    />
  )
  expect(
    (view.getByLabelText('Command (CMD)') as HTMLTextAreaElement).value
  ).toBe('["remote-command"]')
})
