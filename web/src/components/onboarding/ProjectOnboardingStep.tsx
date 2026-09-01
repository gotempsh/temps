// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { GitImportClone } from '@/components/project/GitImportClone'

interface ProjectOnboardingStepProps {
  onSuccess: () => void
}

export function ProjectOnboardingStep({
  onSuccess,
}: ProjectOnboardingStepProps) {
  return <GitImportClone mode="inline" onProjectCreated={onSuccess} />
}
