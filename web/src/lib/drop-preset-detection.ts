import type { DropInspectionResponse } from '@/api/client'
import { prepareDrop, type DropFile } from '@/lib/drop-archive'

export interface PreparedDropInspection {
  archive: File
  inspection: DropInspectionResponse
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
