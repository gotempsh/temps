// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render, screen } from '@testing-library/react'
import type { PresetResponse, ProjectPresetResponse } from '@/api/client'
import { PresetContext } from '@/contexts/PresetContext-shared'
import { installBrowserWindow } from '@/test/dom'
import { FrameworkSelector } from './FrameworkSelector'

installBrowserWindow()
afterEach(cleanup)

const detected = [
  { preset: 'dockerfile', presetLabel: 'Dockerfile', path: './' },
  {
    preset: 'dockerfile',
    presetLabel: 'Dockerfile',
    path: './',
    dockerfilePath: 'docker/Dockerfile',
  },
] as unknown as ProjectPresetResponse[]

function setup(selectedDockerfilePath?: string) {
  const picks: { value: string; card?: ProjectPresetResponse }[] = []
  let parentKeys = 0
  render(
    <PresetContext.Provider
      value={{
        presets: [] as PresetResponse[],
        isLoading: false,
        error: null,
        getPresetBySlug: () => undefined,
      }}
    >
      <div
        onKeyDown={() => {
          parentKeys += 1
        }}
      >
        <FrameworkSelector
          presetData={{ presets: detected }}
          isLoading={false}
          selectedPreset="dockerfile::root"
          selectedDockerfilePath={selectedDockerfilePath}
          onSelectPreset={(value, card) => {
            picks.push({ value, card })
          }}
        />
      </div>
    </PresetContext.Provider>
  )
  return { picks, parentKeys: () => parentKeys }
}

const pressed = () =>
  screen.getAllByRole('button', { pressed: true, name: /Dockerfile/ })

test('choosing a card with Enter stays inside the card', () => {
  const { picks, parentKeys } = setup()
  fireEvent.keyDown(pressed()[0], { key: 'Enter' })
  expect(picks.length).toBe(1)
  expect(parentKeys()).toBe(0)
})

test('other keys still reach the surrounding form', () => {
  const { picks, parentKeys } = setup()
  fireEvent.keyDown(pressed()[0], { key: 'Tab' })
  expect(picks.length).toBe(0)
  expect(parentKeys()).toBe(1)
})

test('tells apart Dockerfiles at one path, and hands back the card chosen', () => {
  const { picks } = setup('docker/Dockerfile')
  expect(pressed().length).toBe(1)
  expect(pressed()[0].textContent).toContain('docker/Dockerfile')
  fireEvent.click(pressed()[0])
  expect(picks[0]).toEqual({ value: 'dockerfile::root', card: detected[1] })
})

test('keeps a hand-set Dockerfile on the list, selected', () => {
  setup('deploy/custom.Dockerfile')
  expect(pressed().length).toBe(1)
  expect(pressed()[0].textContent).toContain('deploy/custom.Dockerfile')
})

test('without a known file every card at the path reads as selected', () => {
  setup()
  expect(pressed().length).toBe(2)
})
