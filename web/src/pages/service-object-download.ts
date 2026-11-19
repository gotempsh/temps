// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Downloads one object from a service's data browser and saves it in the
 * browser. A refused download (missing object, revoked credentials, an
 * unreachable bucket) is reported with the server's own reason instead of
 * only reaching the console log.
 */

import { getErrorMessage } from '@/utils/errorHandling'

export interface ServiceObjectDownload {
  /** Fetches the object body; must reject when the server refuses. */
  fetchObject: () => Promise<unknown>
  /** Saves the bytes under `fileName` (an anchor click in the browser). */
  save: (blob: Blob, fileName: string) => void
  /** Reports a failure to the user (a toast in the console). */
  reportFailure: (title: string, description: string) => void
  fileName: string
}

export function objectBodyAsBlob(data: unknown): Blob {
  if (data instanceof Blob) return data
  if (typeof data === 'string' || data instanceof ArrayBuffer) {
    return new Blob([data], { type: 'application/octet-stream' })
  }
  if (data === undefined || data === null) {
    throw new Error('The server returned an empty response for this object.')
  }
  return new Blob([JSON.stringify(data)], { type: 'application/json' })
}

/** Resolves `true` when the object was saved, `false` when it was reported. */
export async function downloadServiceObject({
  fetchObject,
  save,
  reportFailure,
  fileName,
}: ServiceObjectDownload): Promise<boolean> {
  try {
    save(objectBodyAsBlob(await fetchObject()), fileName)
    return true
  } catch (error) {
    reportFailure(
      `Failed to download ${fileName}`,
      getErrorMessage(error, 'The server did not return the object.')
    )
    return false
  }
}

export function saveBlobInBrowser(blob: Blob, fileName: string): void {
  const url = window.URL.createObjectURL(blob)
  const link = document.createElement('a')
  link.href = url
  link.download = fileName
  document.body.appendChild(link)
  link.click()
  document.body.removeChild(link)
  window.URL.revokeObjectURL(url)
}
