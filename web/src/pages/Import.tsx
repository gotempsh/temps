// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ImportWizard } from '@/components/imports/ImportWizard'
import { useNavigate, useSearchParams } from 'react-router'
import { usePageTitle } from '@/hooks/usePageTitle'

export default function Import() {
  usePageTitle('Import Project')
  const navigate = useNavigate()
  // Deep links (onboarding tiles, docs) preselect the platform via ?source=
  const [searchParams] = useSearchParams()
  const initialSource = searchParams.get('source') ?? undefined

  return (
    <div className="container mx-auto py-8 px-4 sm:px-6 max-w-screen-2xl">
      <ImportWizard
        initialSource={initialSource}
        onCancel={() => navigate('/projects')}
      />
    </div>
  )
}
