import { GitImportClone } from '@/components/project/GitImportClone'

interface ProjectOnboardingStepProps {
  onSuccess: () => void
}

export function ProjectOnboardingStep({
  onSuccess,
}: ProjectOnboardingStepProps) {
  return <GitImportClone mode="inline" onProjectCreated={onSuccess} />
}
