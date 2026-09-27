// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ContainerInfoResponse } from '@/api/client'

export type ContainerPublicUrl = { port?: number; url: string }

/**
 * Every public URL of a Compose service container. A service can expose
 * several public ports, each with its own URL; older servers only return the
 * single `service_url`.
 */
export function containerPublicUrls(
  container: ContainerInfoResponse | null | undefined
): ContainerPublicUrl[] {
  if (!container) return []
  if (container.service_urls?.length) return container.service_urls
  return container.service_url ? [{ url: container.service_url }] : []
}
