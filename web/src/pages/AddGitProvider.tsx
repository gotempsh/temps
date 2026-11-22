// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect } from 'react'
import { useNavigate } from 'react-router'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { Button } from '@/components/ui/button'
import { PageHeader } from '@/components/layout/PageContainer'
import { GitProviderFlow } from '@/components/git-providers/GitProviderFlow'
import { ArrowLeft } from 'lucide-react'
import { useFeedback } from '@/hooks/useFeedback'
import { FeedbackAlert } from '@/components/ui/feedback-alert'

export function AddGitProvider() {
  const navigate = useNavigate()
  const { setBreadcrumbs } = useBreadcrumbs()
  const { feedback, showSuccess, clearFeedback } = useFeedback()

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Git providers', href: '/git-providers' },
      { label: 'Add provider' },
    ])
  }, [setBreadcrumbs])

  usePageTitle('Add Git provider')

  const handleSuccess = () => {
    showSuccess('Git provider added successfully!')
    setTimeout(() => {
      navigate('/git-providers')
    }, 1500)
  }

  const handleCancel = () => {
    navigate('/git-providers')
  }

  return (
    <div className="flex-1 overflow-auto">
      <div className="space-y-6 p-4 sm:p-6 lg:p-8">
        <div className="flex items-start gap-4">
          <Button
            aria-label="Back"
            variant="ghost"
            size="icon"
            onClick={() => navigate('/git-providers')}
            className="shrink-0"
          >
            <ArrowLeft className="h-4 w-4" />
          </Button>
          <PageHeader
            className="min-w-0 flex-1"
            title="Add Git provider"
            description="Connect a Git provider to deploy and manage your services"
          />
        </div>

        {/* Feedback Alert */}
        <FeedbackAlert feedback={feedback} onDismiss={clearFeedback} />

        <GitProviderFlow
          onSuccess={handleSuccess}
          onCancel={handleCancel}
          mode="settings"
        />
      </div>
    </div>
  )
}
