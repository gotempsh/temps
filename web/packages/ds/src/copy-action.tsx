// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// `CopyButton` (web/src/components/ui/copy-button.tsx) already answers
// "copied" on itself via its own tooltip state — never a toast — so it's
// exactly `CopyAction`; wrapped, not duplicated.
export { CopyButton as CopyAction } from '../../../src/components/ui/copy-button'
