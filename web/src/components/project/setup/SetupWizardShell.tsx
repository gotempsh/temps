// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// `SetupWizardShell` is promoted into @temps-sdk/ds as `Wizard` — this file
// is now a thin re-export so existing `@/components/project/setup/SetupWizardShell`
// call sites keep working unchanged. `WizardStepId` stays here: it's a
// setup-flow-specific step-id union, not part of the generic `Wizard` shell.
export { Wizard as SetupWizardShell } from '@temps-sdk/ds'

export type WizardStepId = 'framework' | 'install' | 'waiting'
