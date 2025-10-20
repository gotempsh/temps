import { memo } from 'react'
import { useFormContext } from 'react-hook-form'
import { Card, CardHeader, CardTitle, CardContent } from '@/components/ui/card'
import { Badge } from '@/components/ui/badge'
import { Checkbox } from '@/components/ui/checkbox'
import { ServiceLogo } from '@/components/ui/service-logo'

interface ReviewStepProps {
  existingServices?: any[]
  newlyCreatedServiceIds: number[]
}

export const ReviewStep = memo(function ReviewStep({
  existingServices,
  newlyCreatedServiceIds,
}: ReviewStepProps) {
  const form = useFormContext()
  const formData = form.getValues()

  return (
    <div className="space-y-6">
      <div className="grid gap-4">
        {/* Project Details Summary */}
        <Card>
          <CardHeader>
            <CardTitle className="text-lg">Project Details</CardTitle>
          </CardHeader>
          <CardContent className="space-y-3">
            <div className="grid grid-cols-2 gap-4">
              <div>
                <p className="text-sm font-medium">Name</p>
                <p className="text-sm text-muted-foreground">{formData.name}</p>
              </div>
              <div>
                <p className="text-sm font-medium">Branch</p>
                <p className="text-sm text-muted-foreground">
                  {formData.branch}
                </p>
              </div>
              <div>
                <p className="text-sm font-medium">Framework</p>
                <p className="text-sm text-muted-foreground">
                  {formData.preset}
                </p>
              </div>
              <div>
                <p className="text-sm font-medium">Root Directory</p>
                <p className="text-sm text-muted-foreground">
                  {formData.rootDirectory}
                </p>
              </div>
            </div>
            <div className="flex items-center gap-2">
              <Checkbox checked={formData.autoDeploy} disabled />
              <span className="text-sm">Automatic deployments enabled</span>
            </div>
          </CardContent>
        </Card>

        {/* Services Summary */}
        <Card>
          <CardHeader>
            <CardTitle className="text-lg">Services</CardTitle>
          </CardHeader>
          <CardContent>
            {(formData.storageServices?.length || 0) +
              newlyCreatedServiceIds.length >
            0 ? (
              <div className="space-y-2">
                <p className="text-sm text-muted-foreground">
                  {(formData.storageServices?.length || 0) +
                    newlyCreatedServiceIds.length}{' '}
                  service(s) will be linked to this project
                </p>
                {existingServices &&
                  formData.storageServices?.map((serviceId: number) => {
                    const service = existingServices.find(
                      (s: any) => s.id === serviceId
                    )
                    return service ? (
                      <div
                        key={serviceId}
                        className="flex items-center gap-2 text-sm"
                      >
                        <ServiceLogo service={service.service_type} />
                        <span>{service.name}</span>
                        <Badge variant="outline" className="text-xs">
                          existing
                        </Badge>
                      </div>
                    ) : null
                  })}
                {newlyCreatedServiceIds.length > 0 && (
                  <p className="text-sm text-muted-foreground">
                    + {newlyCreatedServiceIds.length} new service(s) will be
                    created
                  </p>
                )}
              </div>
            ) : (
              <p className="text-sm text-muted-foreground">
                No services selected
              </p>
            )}
          </CardContent>
        </Card>

        {/* Environment Variables Summary */}
        <Card>
          <CardHeader>
            <CardTitle className="text-lg">Environment Variables</CardTitle>
          </CardHeader>
          <CardContent>
            {formData.environmentVariables &&
            formData.environmentVariables.length > 0 ? (
              <div className="space-y-2">
                <p className="text-sm text-muted-foreground">
                  {formData.environmentVariables.length} environment variable(s)
                  configured
                </p>
                {formData.environmentVariables.map(
                  (envVar: any, index: number) => (
                    <div
                      key={index}
                      className="flex items-center gap-2 text-sm"
                    >
                      <span className="font-medium">{envVar.key}</span>
                      {envVar.isSecret && (
                        <Badge variant="secondary" className="text-xs">
                          secret
                        </Badge>
                      )}
                    </div>
                  )
                )}
              </div>
            ) : (
              <p className="text-sm text-muted-foreground">
                No environment variables configured
              </p>
            )}
          </CardContent>
        </Card>
      </div>
    </div>
  )
})
