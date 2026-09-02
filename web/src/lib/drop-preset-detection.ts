// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { DropInspectionResponse, DropPresetCandidate } from '@/api/client'
import { prepareDrop, type DropFile } from '@/lib/drop-archive'

export interface PreparedDropInspection {
  archive: File
  inspection: DropInspectionResponse
}

export function presetConfigForDropCandidate(
  candidate: DropPresetCandidate
): { composePath: string } | { dockerfilePath: string } | undefined {
  if (candidate.preset === 'docker-compose') {
    return candidate.composePath
      ? { composePath: candidate.composePath }
      : undefined
  }
  // A Dockerfile detected in a subdirectory but rolled up to the repository
  // root (e.g. `docker/Dockerfile` with COPY instructions that reach back to
  // the repo root) needs its actual location threaded through so the build
  // still finds it, while the build context stays at the root — leaving
  // `buildContext` unset makes the backend default it to the project's
  // directory, which is already the root here.
  if (candidate.preset === 'dockerfile' && candidate.dockerfilePath) {
    return { dockerfilePath: candidate.dockerfilePath }
  }
  return undefined
}

interface PrepareAndInspectOptions {
  signal?: AbortSignal
  onArchivePrepared?: (archive: File) => void
}

export async function prepareAndInspectDrop(
  files: DropFile[],
  rootPage: string | undefined,
  inspect: (archive: File) => Promise<DropInspectionResponse | undefined>,
  options: PrepareAndInspectOptions = {}
): Promise<PreparedDropInspection> {
  const prepared = await prepareDrop(files, rootPage, options.signal)
  options.onArchivePrepared?.(prepared.file)
  const inspection = await inspect(prepared.file)

  if (!inspection) {
    throw new Error('Preset detection returned no result')
  }
  if (inspection.candidates.length === 0) {
    throw new Error('No deployable project preset was detected')
  }

  return { archive: prepared.file, inspection }
}
