import { memo } from 'react'
import { useFormContext } from 'react-hook-form'
import {
  FormField,
  FormItem,
  FormLabel,
  FormControl,
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Checkbox } from '@/components/ui/checkbox'
import { Badge } from '@/components/ui/badge'
import { RepositoryResponse } from '@/api/client/types.gen'

interface ProjectDetailsStepProps {
  repository: RepositoryResponse
  branches?: any[]
  presetData?: any
}

export const ProjectDetailsStep = memo(function ProjectDetailsStep({
  repository,
  branches,
  presetData,
}: ProjectDetailsStepProps) {
  const form = useFormContext()

  return (
    <div className="space-y-4">
      <FormField
        control={form.control}
        name="name"
        render={({ field }) => (
          <FormItem>
            <FormLabel>Project Name</FormLabel>
            <FormControl>
              <Input {...field} placeholder="Enter project name" />
            </FormControl>
            <FormMessage />
          </FormItem>
        )}
      />

      <FormField
        control={form.control}
        name="branch"
        render={({ field }) => (
          <FormItem>
            <FormLabel>Branch</FormLabel>
            <Select value={field.value} onValueChange={field.onChange}>
              <SelectTrigger>
                <SelectValue placeholder="Select a branch" />
              </SelectTrigger>
              <SelectContent>
                {branches?.map((branch: any) => (
                  <SelectItem key={branch.name} value={branch.name}>
                    {branch.name}
                    {branch.name === repository.default_branch && (
                      <Badge variant="secondary" className="ml-2 text-xs">
                        default
                      </Badge>
                    )}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <FormMessage />
          </FormItem>
        )}
      />

      <FormField
        control={form.control}
        name="preset"
        render={({ field }) => {
          // Create a properly formatted value for the select
          const currentPreset = field.value
          const currentPath =
            form.getValues('rootDirectory')?.replace('./', '') || ''
          const selectValue = currentPreset
            ? `${currentPreset}::${currentPath || 'root'}`
            : ''

          // Determine available options
          const hasPresetData = presetData && presetData.presets?.length > 0

          return (
            <FormItem>
              <FormLabel>Framework Preset</FormLabel>
              <Select
                value={selectValue}
                onValueChange={(value) => {
                  const [presetName, presetPath] = value.split('::')
                  field.onChange(presetName)

                  if (presetPath && presetPath !== 'root') {
                    form.setValue('rootDirectory', `./${presetPath}`)
                  } else {
                    form.setValue('rootDirectory', './')
                  }
                }}
              >
                <SelectTrigger>
                  <SelectValue
                    placeholder={
                      hasPresetData
                        ? 'Select a framework'
                        : 'Loading presets...'
                    }
                  />
                </SelectTrigger>
                <SelectContent>
                  {presetData?.presets?.map((preset: any, index: number) => (
                    <SelectItem
                      key={`preset-${index}-${preset.preset}-${preset.path || './'}`}
                      value={`${preset.preset}::${preset.path || './'}`}
                    >
                      <div className="flex flex-col">
                        <span>{preset.preset_label || preset.preset}</span>
                        <span className="text-xs text-muted-foreground">
                          {preset.path || './'}
                        </span>
                      </div>
                    </SelectItem>
                  ))}
                  {/* Fallback options if no presets are detected */}
                  {!hasPresetData && (
                    <>
                      <SelectItem value="nextjs::root">
                        <div className="flex flex-col">
                          <span>nextjs</span>
                          <span className="text-xs text-muted-foreground">
                            ./
                          </span>
                        </div>
                      </SelectItem>
                      <SelectItem value="vite::root">
                        <div className="flex flex-col">
                          <span>vite</span>
                          <span className="text-xs text-muted-foreground">
                            ./
                          </span>
                        </div>
                      </SelectItem>
                      <SelectItem value="react::root">
                        <div className="flex flex-col">
                          <span>react</span>
                          <span className="text-xs text-muted-foreground">
                            ./
                          </span>
                        </div>
                      </SelectItem>
                      <SelectItem value="static::root">
                        <div className="flex flex-col">
                          <span>static</span>
                          <span className="text-xs text-muted-foreground">
                            ./
                          </span>
                        </div>
                      </SelectItem>
                    </>
                  )}
                </SelectContent>
              </Select>
              <FormMessage />
            </FormItem>
          )
        }}
      />

      <FormField
        control={form.control}
        name="rootDirectory"
        render={({ field }) => (
          <FormItem>
            <FormLabel>Root Directory</FormLabel>
            <FormControl>
              <Input
                {...field}
                placeholder="./"
                readOnly
                className="bg-muted"
              />
            </FormControl>
            <p className="text-xs text-muted-foreground">
              Directory will be set based on the selected framework preset
            </p>
            <FormMessage />
          </FormItem>
        )}
      />

      <FormField
        control={form.control}
        name="autoDeploy"
        render={({ field }) => (
          <FormItem className="flex flex-row items-start space-x-3 space-y-0 rounded-md border p-4">
            <FormControl>
              <Checkbox
                checked={field.value}
                onCheckedChange={field.onChange}
              />
            </FormControl>
            <div className="space-y-1 leading-none">
              <FormLabel>Automatic Deployments</FormLabel>
              <p className="text-sm text-muted-foreground">
                Automatically deploy when changes are pushed to the repository
              </p>
            </div>
          </FormItem>
        )}
      />
    </div>
  )
})
