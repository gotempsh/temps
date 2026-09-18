// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { RepositoryCatalogPlugin } from '@/api/client/types.gen'

export function filterRepositoryCatalog(
  plugins: RepositoryCatalogPlugin[],
  search: string,
  category: string
) {
  const needle = search.trim().toLowerCase()
  return plugins.filter(
    (plugin) =>
      (category === '' || plugin.category === category) &&
      [
        plugin.name,
        plugin.title,
        plugin.summary,
        plugin.author,
        plugin.repository,
      ].some((value) => value.toLowerCase().includes(needle))
  )
}
