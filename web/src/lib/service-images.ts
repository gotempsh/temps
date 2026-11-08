// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Keep console-side fallbacks aligned with the managed RustFS provider. */
export const DEFAULT_RUSTFS_IMAGE = 'rustfs/rustfs:1.0.0'

/** Keep aligned with `DEFAULT_GARAGE_IMAGE` in the managed Garage provider. */
export const DEFAULT_GARAGE_IMAGE = 'dxflrs/garage:v2.1.0'

/**
 * Keep aligned with `DEFAULT_REDIS_IMAGE` in the managed Redis provider: the
 * GHCR image rebuilt weekly on Redis's trixie base. Upstream stopped shipping
 * bookworm builds after 8.4.0, so the old `8-bookworm` tag is unpatched.
 */
export const DEFAULT_REDIS_IMAGE = 'ghcr.io/gotempsh/redis-walg:8-trixie'
