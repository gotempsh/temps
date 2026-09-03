// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export type ProjectSettingsView = 'git' | 'build'

export function projectSettingsSections(
  view: ProjectSettingsView,
  sourceType: string | null | undefined
) {
  const isUploadedSource = sourceType === 'uploaded_source'

  return {
    showRepository: view === 'git' && !isUploadedSource,
    showUploadedSource: view === 'git' && isUploadedSource,
    showBuildConfiguration: view === 'build',
    showGitAutomation: view === 'git' && !isUploadedSource,
  }
}
