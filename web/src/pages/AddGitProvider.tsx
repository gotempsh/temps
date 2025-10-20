import { useEffect } from 'react'
import { useNavigate } from 'react-router-dom'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { GitProviderFlow } from '@/components/git-providers/GitProviderFlow'
import { ArrowLeft } from 'lucide-react'
import { useFeedback } from '@/hooks/useFeedback'
import { FeedbackAlert } from '@/components/ui/feedback-alert'

export function AddGitProvider() {
  const navigate = useNavigate()
  const { setBreadcrumbs } = useBreadcrumbs()
  const { feedback, showSuccess, showError, clearFeedback } = useFeedback()

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Git Sources', href: '/git-sources' },
      { label: 'Add Provider' },
    ])
  }, [setBreadcrumbs])

  usePageTitle('Add Git Provider')

  const handleSuccess = () => {
    showSuccess('Git provider added successfully!')
    setTimeout(() => {
      navigate('/git-sources')
    }, 1500)
  }

  const handleCancel = () => {
    navigate('/git-sources')
  }

  return (
    <div className="flex-1 overflow-auto">
      <div className="space-y-6 p-4 sm:p-6 lg:p-8">
        <div className="flex items-center gap-4">
          <Button
            variant="ghost"
            size="icon"
            onClick={() => navigate('/git-sources')}
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

        <Card className="w-full">
          <CardHeader className="px-4 sm:px-6">
            <CardTitle>Select and Connect Provider</CardTitle>
          </CardHeader>
          <CardContent className="px-4 sm:px-6">
            <GitProviderFlow
              onSuccess={handleSuccess}
              onCancel={handleCancel}
              mode="settings"
            />
          </CardContent>
        </Card>
      </div>
    </div>
  )
}
