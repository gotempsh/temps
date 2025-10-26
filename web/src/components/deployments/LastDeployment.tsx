import { DeploymentResponse } from '@/api/client'
import { getSettingsOptions } from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { ReloadableImage } from '@/components/utils/ReloadableImage'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { useQuery } from '@tanstack/react-query'
import { Camera, ExternalLink, GitBranch, Settings } from 'lucide-react'
import { Link } from 'react-router-dom'
import { DeploymentStatusBadge } from '../deployment/DeploymentStatusBadge'

interface LastDeploymentProps {
  deployment: DeploymentResponse
  projectName: string
}

export function LastDeployment({
  deployment,
  projectName,
}: LastDeploymentProps) {
  // Fetch platform settings to check if screenshots are enabled
  const { data: settings } = useQuery({
    ...getSettingsOptions(),
    retry: false,
  })

  const screenshotsEnabled = settings?.screenshots?.enabled ?? false

  return (
    <Card>
      <CardContent className="p-6">
        <div className="flex flex-col md:flex-row gap-6 md:gap-4">
          <div className="w-full md:w-1/3">
            {!screenshotsEnabled ? (
              <div className="flex items-center justify-center">
                <Card className="w-full bg-muted/50 border-dashed">
                  <CardContent className="flex flex-col items-center justify-center h-48 text-center p-4">
                    <Camera className="h-8 w-8 text-muted-foreground mb-2" />
                    <p className="text-sm text-muted-foreground mb-3">
                      Screenshot generation is disabled
                    </p>
                    <Link to="/settings">
                      <Button variant="outline" size="sm">
                        <Settings className="h-3 w-3 mr-1" />
                        Enable in Settings
                      </Button>
                    </Link>
                  </CardContent>
                </Card>
              </div>
            ) : deployment.screenshot_location ? (
              <ReloadableImage
                src={`/api/files${deployment.screenshot_location?.startsWith('/') ? deployment.screenshot_location : '/' + deployment.screenshot_location}`}
                alt={`${projectName} deployment ${deployment.id}`}
                className="w-full rounded-md"
              />
            ) : deployment.status === 'failed' ? (
              <div className="flex items-center justify-center">
                <Card className="w-full max-w-md bg-gray-900 border-gray-800">
                  <CardContent className="flex items-center justify-center h-48">
                    <p className="text-gray-400 text-lg">Failed to deploy</p>
                  </CardContent>
                </Card>
              </div>
            ) : (
              <div className="flex items-center justify-center">
                <Card className="w-full max-w-md bg-gray-900 border-gray-800">
                  <CardContent className="flex items-center justify-center h-48">
                    <p className="text-gray-400 text-lg">
                      {deployment.status === 'completed'
                        ? 'Generating screenshot...'
                        : 'Building...'}
                    </p>
                  </CardContent>
                </Card>
              </div>
            )}
          </div>
          <div className="w-full md:w-2/3">
            <h3 className="text-lg font-semibold mb-2">
              Deployment Information
            </h3>
            <div className="flex flex-col items-start gap-2 mb-4">
              {deployment.environment.domains.map((domain) => {
                const url = domain.startsWith('http')
                  ? domain
                  : `https://${domain}`
                return (
                  <div
                    key={domain}
                    className="flex items-center gap-1 cursor-pointer hover:opacity-80 transition-opacity"
                    onClick={() => window.open(url, '_blank')}
                  >
                    <span className="text-sm text-muted-foreground truncate">
                      {domain}
                    </span>
                    <ExternalLink className="h-3.5 w-3.5 text-muted-foreground shrink-0" />
                  </div>
                )
              })}
              <div
                className="flex items-center gap-1 cursor-pointer hover:opacity-80 transition-opacity"
                onClick={() => window.open(deployment.url, '_blank')}
              >
                <span className="text-sm text-muted-foreground truncate">
                  {deployment.url}
                </span>
                <ExternalLink className="h-3.5 w-3.5 text-muted-foreground shrink-0" />
              </div>
            </div>

            <h4 className="text-sm font-semibold mb-2">Status</h4>
            <div className="flex items-center mb-4">
              <DeploymentStatusBadge deployment={deployment} className="mr-2" />
              <span className="text-xs text-muted-foreground">
                <TimeAgo date={deployment.created_at} /> by{' '}
                {deployment.commit_author}
              </span>
            </div>

            <h4 className="text-sm font-semibold mb-2">Source</h4>
            <div className="text-sm space-y-1">
              <p className="flex items-center text-muted-foreground">
                <GitBranch className="mr-2 h-4 w-4" />
                {deployment.branch}
              </p>
              <p className="flex items-start text-muted-foreground">
                {deployment.commit_hash?.slice(0, 7)}&nbsp;
                <span className="font-medium text-foreground ml-1">
                  {deployment.commit_message}
                </span>
              </p>
            </div>
          </div>
        </div>
      </CardContent>
    </Card>
  )
}
