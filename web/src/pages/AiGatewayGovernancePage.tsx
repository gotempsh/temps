import { usePageTitle } from '@/hooks/usePageTitle'
import { GovernanceSettings } from './AiGateway'

export function AiGatewayGovernancePage() {
  usePageTitle('AI Governance')

  return (
    <div className="container mx-auto px-4 sm:px-6 py-4 sm:py-6 space-y-4 sm:space-y-6">
      <div>
        <h1 className="text-2xl sm:text-3xl font-bold">AI Governance</h1>
        <p className="text-muted-foreground mt-1 sm:mt-2 text-sm">
          Model allowlists, request-rate limits, and monthly cost budgets per
          instance, project, or environment.
        </p>
      </div>
      <GovernanceSettings />
    </div>
  )
}
