// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ExecResponse } from '@/api/client/types.gen'
import { getErrorMessage } from '@/utils/errorHandling'

/**
 * The result to show when a sandbox command could not run at all.
 *
 * The API client throws the parsed response body, so the error is usually a
 * problem document (`{ title, detail }`), not an `Error` with a `message`.
 * Reading `.message` from it gave `undefined` output, which crashed the page.
 */
export function execFailureResult(error: unknown): ExecResponse {
  return {
    exit_code: -1,
    stdout: '',
    stderr: getErrorMessage(error, 'The command could not run'),
  }
}
