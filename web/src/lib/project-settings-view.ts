// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export type ProjectSettingsView = 'git' | 'build'

export function projectSettingsSections(
  view: ProjectSettingsView,
  sourceType: string | null | undefined
) {
  const isUploadedSource = sourceType === 'uploaded_source'
  const isComposeSource = sourceType === 'compose'
  const isLocalSource = isUploadedSource || isComposeSource

  return {
    showRepository: view === 'git' && !isLocalSource,
    showUploadedSource: view === 'git' && isLocalSource,
    showBuildConfiguration: view === 'build',
    showGitAutomation: view === 'git' && !isLocalSource,
  }
}
