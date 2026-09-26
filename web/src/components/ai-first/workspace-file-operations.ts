// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export class LatestWorkspaceRequest {
  private generation = 0

  begin(): number {
    this.generation += 1
    return this.generation
  }

  invalidate(): void {
    this.generation += 1
  }

  isCurrent(request: number): boolean {
    return request === this.generation
  }
}

export function workspaceFileExplorerKey(applicationPublicId?: string): string {
  return applicationPublicId ?? 'global-workspace'
}

export function nextWorkspaceFileRevision(current: number): number {
  return current + 1
}

export async function uploadWorkspaceBatches<T>(
  batches: T[][],
  upload: (batch: T[]) => Promise<void>
): Promise<{ completed: number; error: unknown | null }> {
  let completed = 0
  try {
    for (const batch of batches) {
      await upload(batch)
      completed += batch.length
    }
    return { completed, error: null }
  } catch (error) {
    return { completed, error }
  }
}
