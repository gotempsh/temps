// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ImportWizard } from '@/components/imports/ImportWizard'
import { useNavigate, useSearchParams } from 'react-router'
import { usePageTitle } from '@/hooks/usePageTitle'
import { usePlatformFeatures } from '@/hooks/usePlatformFeatures'
import { PlatformFeatureNotice } from '@/components/platform/PlatformFeatureNotice'

export default function Import() {
  usePageTitle('Import Project')
  const navigate = useNavigate()
  const platformFeatures = usePlatformFeatures()
  // Deep links (onboarding tiles, docs) preselect the platform via ?source=
  const [searchParams] = useSearchParams()
  const initialSource = searchParams.get('source') ?? undefined

  return (
    <div className="w-full space-y-4 px-4 py-8 sm:px-6 lg:px-8">
      {platformFeatures.data && (
        <PlatformFeatureNotice
          available={platformFeatures.data.imports}
          label="Workload importers"
        />
      )}
      <ImportWizard
        initialSource={initialSource}
        onCancel={() => navigate('/projects')}
      />
    </div>
  )
}
