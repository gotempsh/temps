// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// `LogLine` is promoted into @temps-sdk/ds as the canonical implementation
// (see web/packages/ds/src/log-line.tsx) — this file is now a thin re-export
// so existing `@/components/runtime-logs/log-line` call sites keep working
// unchanged.
export { LogLine, type LogLineProps } from '@temps-sdk/ds'
