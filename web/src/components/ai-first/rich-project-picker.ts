// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export type ProjectPickerTone = 'healthy' | 'warning' | 'down' | 'neutral'

export type ProjectPickerItem = {
  id: number
  name: string
  slug: string
  status: string
  tone: ProjectPickerTone
}

export function projectFaviconUrl(projectId: number): string {
  return `/api/projects/${projectId}/favicon`
}
