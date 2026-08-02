import { useEffect } from 'react'
import { useNavigate } from 'react-router'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { Button } from '@/components/ui/button'
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
      { label: 'Git Providers', href: '/git-providers' },
      { label: 'Add Provider' },
    ])
  }, [setBreadcrumbs])

  usePageTitle('Add Git Provider')

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
        <div className="flex items-center gap-4">
          <Button
            variant="ghost"
            size="icon"
            onClick={() => navigate('/git-providers')}
            className="shrink-0"
          >
            <ArrowLeft className="h-4 w-4" />
          </Button>
          <div>
            <h1 className="text-xl sm:text-2xl font-bold">Add Git Provider</h1>
            <p className="text-sm sm:text-base text-muted-foreground">
              Connect a Git provider to deploy and manage your projects
            </p>
          </div>
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
