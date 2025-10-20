import { memo } from 'react'
import { useFormContext } from 'react-hook-form'
import {
  FormField,
  FormItem,
  FormLabel,
  FormControl,
  FormMessage,
} from '@/components/ui/form'
import { Card, CardContent } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Checkbox } from '@/components/ui/checkbox'
import { Plus, X, Eye, EyeOff, Settings } from 'lucide-react'

interface EnvironmentStepProps {
  watchedEnvVars: any[]
  showSecrets: { [key: number]: boolean }
  onAddVariable: () => void
  onRemoveVariable: (index: number) => void
  onToggleSecret: (index: number) => void
}

export const EnvironmentStep = memo(function EnvironmentStep({
  watchedEnvVars,
  showSecrets,
  onAddVariable,
  onRemoveVariable,
  onToggleSecret,
}: EnvironmentStepProps) {
  const form = useFormContext()

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <p className="text-sm text-muted-foreground">
          Add environment variables that will be available at runtime
        </p>
        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={onAddVariable}
        >
          <Plus className="h-4 w-4 mr-2" />
          Add Variable
        </Button>
      </div>

      {watchedEnvVars.length > 0 && (
        <div className="space-y-3">
          {watchedEnvVars.map((_, index) => (
            <Card key={index} className="border-dashed">
              <CardContent className="p-4">
                <div className="flex items-start gap-3">
                  <div className="flex-1 grid grid-cols-1 md:grid-cols-2 gap-3">
                    <FormField
                      control={form.control}
                      name={`environmentVariables.${index}.key`}
                      render={({ field }) => (
                        <FormItem>
                          <FormLabel className="text-sm">Key</FormLabel>
                          <FormControl>
                            <Input {...field} placeholder="DATABASE_URL" />
                          </FormControl>
                          <FormMessage />
                        </FormItem>
                      )}
                    />
                    <FormField
                      control={form.control}
                      name={`environmentVariables.${index}.value`}
                      render={({ field }) => (
                        <FormItem>
                          <FormLabel className="text-sm">Value</FormLabel>
                          <div className="relative">
                            <FormControl>
                              <Input
                                {...field}
                                type={showSecrets[index] ? 'text' : 'password'}
                                placeholder="Enter value"
                              />
                            </FormControl>
                            <Button
                              type="button"
                              variant="ghost"
                              size="sm"
                              className="absolute right-0 top-0 h-full px-3"
                              onClick={() => onToggleSecret(index)}
                            >
                              {showSecrets[index] ? (
                                <EyeOff className="h-4 w-4" />
                              ) : (
                                <Eye className="h-4 w-4" />
                              )}
                            </Button>
                          </div>
                          <FormMessage />
                        </FormItem>
                      )}
                    />
                  </div>
                  <div className="flex flex-col gap-2">
                    <FormField
                      control={form.control}
                      name={`environmentVariables.${index}.isSecret`}
                      render={({ field }) => (
                        <FormItem className="flex items-center space-x-2 space-y-0">
                          <FormControl>
                            <Checkbox
                              checked={field.value}
                              onCheckedChange={field.onChange}
                            />
                          </FormControl>
                          <FormLabel className="text-xs">Secret</FormLabel>
                        </FormItem>
                      )}
                    />
                    <Button
                      type="button"
                      variant="ghost"
                      size="sm"
                      onClick={() => onRemoveVariable(index)}
                      className="text-destructive hover:text-destructive h-8 w-8 p-0"
                    >
                      <X className="h-4 w-4" />
                    </Button>
                  </div>
                </div>
              </CardContent>
            </Card>
          ))}
        </div>
      )}

      {watchedEnvVars.length === 0 && (
        <Card className="border-dashed">
          <CardContent className="p-8 text-center">
            <Settings className="h-12 w-12 mx-auto text-muted-foreground mb-4" />
            <h3 className="font-medium mb-2">No environment variables</h3>
            <p className="text-sm text-muted-foreground mb-4">
              Environment variables will be available to your application at
              runtime
            </p>
            <Button type="button" variant="outline" onClick={onAddVariable}>
              <Plus className="h-4 w-4 mr-2" />
              Add Your First Variable
            </Button>
          </CardContent>
        </Card>
      )}
    </div>
  )
})
