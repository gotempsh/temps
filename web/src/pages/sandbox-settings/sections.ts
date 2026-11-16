// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export type SandboxSettingsSectionId = 'runtime' | 'preview'

export const SANDBOX_SETTINGS_PATH = '/sandboxes/settings'

export const SANDBOX_SETTINGS_SECTIONS: ReadonlyArray<{
  id: SandboxSettingsSectionId
  label: string
  title: string
  path: string
}> = [
  {
    id: 'runtime',
    label: 'Runtime',
    title: 'Sandbox runtime',
    path: SANDBOX_SETTINGS_PATH,
  },
  {
    id: 'preview',
    label: 'Preview gateway',
    title: 'Preview gateway',
    path: `${SANDBOX_SETTINGS_PATH}/preview`,
  },
]

/** The settings section a pathname under `/sandboxes/settings` shows. */
export function sandboxSettingsSection(
  pathname: string
): SandboxSettingsSectionId {
  const normalized = pathname.replace(/\/+$/, '')
  return normalized === `${SANDBOX_SETTINGS_PATH}/preview`
    ? 'preview'
    : 'runtime'
}
