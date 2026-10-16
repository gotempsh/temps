// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Read every page of a list endpoint that has no server-side filter and
 * returns a bare array. Stops at the first page shorter than `pageSize`, or
 * after `maxPages` so a misbehaving endpoint cannot loop forever.
 */
export async function fetchAllPages<T>(
  fetchPage: (page: number, pageSize: number) => Promise<T[]>,
  pageSize = 100,
  maxPages = 50
): Promise<T[]> {
  const all: T[] = []
  for (let page = 1; page <= maxPages; page++) {
    const batch = await fetchPage(page, pageSize)
    all.push(...batch)
    if (batch.length < pageSize) break
  }
  return all
}
