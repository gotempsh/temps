// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// `ProjectAvatar` is promoted into @temps-sdk/ds as the canonical
// implementation (see web/packages/ds/src/project-avatar.tsx) — this file is
// now a thin re-export so existing `@/components/project/ProjectAvatar` call
// sites keep working unchanged.
export { ProjectAvatar, type ProjectAvatarProps } from '@temps-sdk/ds'
