// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from '@testing-library/react'
import type {
  PresetResponse,
  ProjectPresetResponse,
  ProjectResponse,
} from '@/api/client'
import { PresetContext } from '@/contexts/PresetContext-shared'
import type { BuildConfigurationPatch } from '@/lib/build-configuration-form'
import { installBrowserWindow } from '@/test/dom'
import { BuildConfigurationCard } from './BuildConfigurationCard'

installBrowserWindow()
afterEach(cleanup)

const catalog = [
  {
    slug: 'nextjs',
    label: 'Next.js',
    project_type: 'server',
    description: 'A React framework',
  },
  {
    slug: 'vite',
    label: 'Vite',
    project_type: 'static',
    description: 'A build tool',
  },
] as unknown as PresetResponse[]

const baseProject = {
  id: 7,
  preset: 'nextjs',
  directory: './apps/web',
  main_branch: 'main',
  source_type: 'git',
  pull_only_root_directory: false,
  preset_config: {},
} as unknown as ProjectResponse

function setup(
  options: {
    project?: Partial<ProjectResponse>
    detected?: ProjectPresetResponse[]
    onSave?: (patch: BuildConfigurationPatch) => Promise<void>
  } = {}
) {
  const saves: BuildConfigurationPatch[] = []
  const onSave =
    options.onSave ??
    (async (patch: BuildConfigurationPatch) => {
      saves.push(patch)
    })
  const view = render(
    <PresetContext.Provider
      value={{
        presets: catalog,
        isLoading: false,
        error: null,
        getPresetBySlug: (slug) => catalog.find((item) => item.slug === slug),
      }}
    >
      <BuildConfigurationCard
        project={{ ...baseProject, ...options.project } as ProjectResponse}
        saving={false}
        onSave={onSave}
        detectedPresets={options.detected}
        isDetecting={false}
        isRefreshingDetection={false}
        detectionError={null}
      />
    </PresetContext.Provider>
  )
  return { view, saves }
}

const saveButton = () =>
  screen.getByRole('button', { name: 'Save' }) as HTMLButtonElement

test('shows every setting without entering an edit mode, and the framework by name', () => {
  setup()
  expect(screen.getByLabelText('Root directory')).toBeTruthy()
  expect(screen.getByText('Next.js')).toBeTruthy()
  expect(screen.queryByText('nextjs')).toBeNull()
  expect(saveButton().disabled).toBe(true)
  expect(screen.getByText('Changes apply on the next deployment.')).toBeTruthy()
})

test('saves several edits together, once', async () => {
  const { saves } = setup()
  fireEvent.change(screen.getByLabelText('Root directory'), {
    target: { value: './apps/admin' },
  })
  fireEvent.click(screen.getByLabelText('Pull only the root directory'))
  await waitFor(() => expect(saveButton().disabled).toBe(false))
  fireEvent.click(saveButton())
  await waitFor(() => expect(saves.length).toBe(1))
  expect(saves[0]).toEqual({
    directory: './apps/admin',
    pull_only_root_directory: true,
  })
})

test('keeps a refused directory in the field and shows the reason beside it', async () => {
  const reason = "Root directory 'apps/nope' does not exist on branch 'main'"
  setup({
    onSave: async () => {
      throw { detail: reason }
    },
  })
  const directory = screen.getByLabelText('Root directory') as HTMLInputElement
  fireEvent.change(directory, { target: { value: 'apps/nope' } })
  await waitFor(() => expect(saveButton().disabled).toBe(false))
  fireEvent.click(saveButton())
  await waitFor(() => expect(screen.getByText(reason)).toBeTruthy())
  expect(directory.value).toBe('apps/nope')
  fireEvent.change(directory, { target: { value: 'apps/web2' } })
  await waitFor(() => expect(screen.queryByText(reason)).toBeNull())
})

test('choosing a framework stages it until Save and keeps the directory', async () => {
  const { saves } = setup()
  fireEvent.click(screen.getByRole('button', { name: 'Change' }))
  const vite = await screen.findByRole('button', { name: /Vite/ })
  fireEvent.keyDown(vite, { key: 'Enter' })
  await waitFor(() => expect(screen.getByText('Vite')).toBeTruthy())
  await waitFor(() =>
    expect(screen.queryByText('Choose a framework')).toBeNull()
  )
  expect(saves.length).toBe(0)
  const directory = screen.getByLabelText('Root directory') as HTMLInputElement
  expect(directory.value).toBe('./apps/web')
  await waitFor(() => expect(saveButton().disabled).toBe(false))
  fireEvent.click(saveButton())
  await waitFor(() => expect(saves.length).toBe(1))
  expect(saves[0]).toEqual({ preset: 'vite' })
})

test('re-clicking the current Dockerfile card leaves a custom path and nothing to save', async () => {
  const { saves } = setup({
    project: {
      preset: 'dockerfile',
      directory: './',
      preset_config: {
        preset: 'dockerfile',
        dockerfilePath: 'docker/custom.Dockerfile',
      },
    } as never,
    detected: [
      { preset: 'dockerfile', presetLabel: 'Dockerfile', path: './' },
    ] as unknown as ProjectPresetResponse[],
  })
  fireEvent.click(screen.getByRole('button', { name: 'Change' }))
  const current = await screen.findByRole('button', {
    name: /docker\/custom\.Dockerfile/,
    pressed: true,
  })
  fireEvent.click(current)
  await waitFor(() =>
    expect(screen.queryByText('Choose a framework')).toBeNull()
  )
  const path = screen.getByLabelText('Dockerfile path') as HTMLInputElement
  expect(path.value).toBe('docker/custom.Dockerfile')
  expect(saveButton().disabled).toBe(true)
  expect(saves.length).toBe(0)
})

test('choosing the other Dockerfile at the same path stages that file', async () => {
  const { saves } = setup({
    project: {
      preset: 'dockerfile',
      directory: './',
      preset_config: {
        preset: 'dockerfile',
        dockerfilePath: 'docker/Dockerfile',
      },
    } as never,
    detected: [
      {
        preset: 'dockerfile',
        presetLabel: 'Dockerfile',
        path: './',
        dockerfilePath: 'docker/Dockerfile',
      },
      {
        preset: 'dockerfile',
        presetLabel: 'Dockerfile',
        path: './',
        dockerfilePath: '.devcontainer/Dockerfile',
      },
    ] as unknown as ProjectPresetResponse[],
  })
  fireEvent.click(screen.getByRole('button', { name: 'Change' }))
  const other = await screen.findByRole('button', {
    name: /\.devcontainer\/Dockerfile/,
    pressed: false,
  })
  expect(
    screen.getAllByRole('button', { name: /Dockerfile/, pressed: true }).length
  ).toBe(1)
  fireEvent.click(other)
  const path = (await screen.findByLabelText(
    'Dockerfile path'
  )) as HTMLInputElement
  await waitFor(() => expect(path.value).toBe('.devcontainer/Dockerfile'))
  await waitFor(() => expect(saveButton().disabled).toBe(false))
  fireEvent.click(saveButton())
  await waitFor(() => expect(saves.length).toBe(1))
  expect(saves[0].preset_config?.dockerfilePath).toBe(
    '.devcontainer/Dockerfile'
  )
})

test('the form cannot be edited while a save is in flight', async () => {
  let finish = () => {}
  setup({
    onSave: () =>
      new Promise<void>((resolve) => {
        finish = resolve
      }),
  })
  const directory = screen.getByLabelText('Root directory') as HTMLInputElement
  fireEvent.change(directory, { target: { value: './apps/admin' } })
  await waitFor(() => expect(saveButton().disabled).toBe(false))
  fireEvent.click(saveButton())
  await waitFor(() => expect(directory.readOnly).toBe(true))
  expect(
    (screen.getByRole('button', { name: 'Change' }) as HTMLButtonElement)
      .disabled
  ).toBe(true)
  expect(
    screen
      .getByLabelText('Pull only the root directory')
      .hasAttribute('disabled')
  ).toBe(true)
  finish()
  await waitFor(() => expect(directory.readOnly).toBe(false))
})

test('offers no clone option for an uploaded source', () => {
  setup({ project: { source_type: 'uploaded_source' } as never })
  expect(screen.queryByLabelText('Pull only the root directory')).toBeNull()
})

test('offers no clone option at the repository root', async () => {
  setup()
  expect(screen.getByLabelText('Pull only the root directory')).toBeTruthy()
  fireEvent.change(screen.getByLabelText('Root directory'), {
    target: { value: './' },
  })
  await waitFor(() =>
    expect(screen.queryByLabelText('Pull only the root directory')).toBeNull()
  )
})

test('shows only the file settings of the selected framework', () => {
  const dockerfile = setup({
    project: { preset: 'dockerfile' } as never,
  })
  expect(screen.getByLabelText('Dockerfile path')).toBeTruthy()
  expect(screen.getByLabelText('Build context')).toBeTruthy()
  expect(screen.queryByLabelText('Compose file')).toBeNull()
  dockerfile.view.unmount()
  setup({ project: { preset: 'docker-compose' } as never })
  expect(screen.getByLabelText('Compose file')).toBeTruthy()
  expect(screen.queryByLabelText('Dockerfile path')).toBeNull()
})
