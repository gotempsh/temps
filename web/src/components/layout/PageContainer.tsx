// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// `PageContainer`/`PageHeader` are promoted into @temps-sdk/ds as the
// canonical implementation (see web/packages/ds/src/page-header.tsx) — this
// file is now a thin re-export so the existing `@/components/layout/
// PageContainer` call sites keep working unchanged. `PageHeader` gained one
// new optional prop (`verdict`) for the record recipe; every existing call
// site is unaffected.
export { PageContainer, PageHeader } from '@temps-sdk/ds'
