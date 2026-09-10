// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export function shouldShowS3SourceHeaderAction(
  isLoading: boolean,
  sourceCount: number
) {
  return !isLoading && sourceCount > 0
}
