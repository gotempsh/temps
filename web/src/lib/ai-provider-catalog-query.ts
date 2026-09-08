// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { listAiProvidersOptions } from '@/api/client/@tanstack/react-query.gen'

/** Shared query identity for every consumer of the provider catalog. */
export const aiProviderCatalogQueryOptions = listAiProvidersOptions()
