// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export type ComposeSourceDraft = {
  projectId: number
  content: string
  baseRevision: number | null
}

export function composeSourceDraftForProject(
  draft: ComposeSourceDraft | null,
  projectId: number
): ComposeSourceDraft | null {
  return draft?.projectId === projectId ? draft : null
}

export function updateComposeSourceDraft(
  draft: ComposeSourceDraft | null,
  projectId: number,
  content: string,
  currentRevision: number | null
): ComposeSourceDraft {
  const current = composeSourceDraftForProject(draft, projectId)
  return {
    projectId,
    content,
    baseRevision: current ? current.baseRevision : currentRevision,
  }
}

export function composeSourceExpectedRevision(
  draft: ComposeSourceDraft | null,
  projectId: number,
  currentRevision: number | null
): number | null {
  const current = composeSourceDraftForProject(draft, projectId)
  return current ? current.baseRevision : currentRevision
}
