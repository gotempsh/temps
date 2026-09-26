// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { TemplateResponse } from '@/api/client/types.gen'
import type { ProjectSource } from '@/components/project/NewProjectShell'

export function templateSource(
  template: Pick<TemplateResponse, 'kind'>
): Extract<ProjectSource, 'services' | 'templates'> {
  return template.kind === 'service' ? 'services' : 'templates'
}

export function templateBelongsToSource(
  template: Pick<TemplateResponse, 'kind'>,
  source: ProjectSource | null
): boolean {
  return templateSource(template) === source
}
