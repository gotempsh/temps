// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ProjectPresetResponse, ProjectResponse } from '@/api/client'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import {
  Form,
  FormControl,
  FormDescription,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import { usePresets } from '@/contexts/PresetContext'
import { problemDetail } from '@/lib/api-problem'
import {
  applyFrameworkSelection,
  buildConfigurationPatch,
  buildConfigurationSchema,
  buildConfigurationValues,
  DEFAULT_COMPOSE_PATH,
  DEFAULT_DOCKERFILE_PATH,
  isComposePreset,
  isDockerfilePreset,
  normalizeBuildConfiguration,
  type BuildConfigurationPatch,
  type BuildConfigurationValues,
} from '@/lib/build-configuration-form'
import {
  isRepositoryRootDirectory,
  rootDirectoryHelp,
} from '@/lib/project-directory'
import { presetSelectionKey } from '@/lib/preset-selection'
import { zodResolver } from '@hookform/resolvers/zod'
import { Loader2 } from 'lucide-react'
import { useState } from 'react'
import { useForm, useWatch } from 'react-hook-form'
import { toast } from 'sonner'
import FrameworkIcon from '../FrameworkIcon'
import { FrameworkSelector } from '../FrameworkSelector'

interface BuildConfigurationCardProps {
  project: ProjectResponse
  /** A save of these settings is in flight, from any form sharing the request. */
  saving: boolean
  /** Rejects with the server's problem when the save is refused. */
  onSave: (patch: BuildConfigurationPatch) => Promise<void>
  detectedPresets: readonly ProjectPresetResponse[] | undefined
  isDetecting: boolean
  isRefreshingDetection: boolean
  detectionError: Error | null
  onRefreshDetection?: () => void
}

/**
 * Framework, source directory and the build files they need, edited and saved
 * as one form like every other group on the Build & deploy page. Choosing a
 * framework stages it with the rest rather than saving on the spot, since it
 * can also change the root directory and the fields below it.
 */
export function BuildConfigurationCard(props: BuildConfigurationCardProps) {
  return <BuildConfigurationForm key={props.project.id} {...props} />
}

function BuildConfigurationForm({
  project,
  saving,
  onSave,
  detectedPresets,
  isDetecting,
  isRefreshingDetection,
  detectionError,
  onRefreshDetection,
}: BuildConfigurationCardProps) {
  const isUploadedSource = project.source_type === 'uploaded_source'
  const [frameworkOpen, setFrameworkOpen] = useState(false)
  const { getPresetBySlug } = usePresets()

  const form = useForm<BuildConfigurationValues>({
    resolver: zodResolver(buildConfigurationSchema),
    resetOptions: { keepDirtyValues: true },
    values: buildConfigurationValues(project),
  })
  const values = useWatch({ control: form.control }) as BuildConfigurationValues
  const patch = buildConfigurationPatch(project, values)
  const dirty = Object.keys(patch).length > 0
  const busy = saving || form.formState.isSubmitting
  const rootError = form.formState.errors.root?.message
  const directoryError = form.formState.errors.directory?.message
  const frameworkLabel =
    getPresetBySlug(values.preset)?.label || values.preset || 'None selected'

  const handleFrameworkSelection = (
    selection: string,
    detected?: ProjectPresetResponse
  ) => {
    const next = applyFrameworkSelection(values, selection, detected)
    for (const name of Object.keys(next) as (keyof typeof next)[]) {
      if (next[name] !== values[name]) {
        form.setValue(name, next[name] as never, { shouldDirty: true })
      }
    }
    form.clearErrors('directory')
    setFrameworkOpen(false)
  }

  const handleSave = async (submitted: BuildConfigurationValues) => {
    const changes = buildConfigurationPatch(project, submitted)
    if (Object.keys(changes).length === 0) return
    try {
      await onSave(changes)
    } catch (error) {
      // The server names what is wrong with a directory it refuses, so keep
      // that beside the field. Edits stay in the form to correct and retry.
      const message = problemDetail(
        error,
        'The build configuration could not be saved.'
      )
      form.setError(changes.directory !== undefined ? 'directory' : 'root', {
        message,
      })
      return
    }
    form.reset(normalizeBuildConfiguration(submitted))
    toast.success('Build configuration saved. Deploy to apply it.')
  }

  return (
    <Form {...form}>
      <form
        id="setting-build-configuration"
        className="scroll-mt-4"
        onSubmit={form.handleSubmit(handleSave)}
      >
        <Card>
          <CardHeader>
            <CardTitle className="text-base">Build configuration</CardTitle>
            <CardDescription>
              The framework Temps builds with, and where it finds your code.
            </CardDescription>
          </CardHeader>
          <CardContent className="space-y-5">
            <div id="setting-framework" className="scroll-mt-4 space-y-2">
              <p
                id="build-configuration-framework"
                className="text-sm font-medium"
              >
                Framework
              </p>
              <div className="flex items-center justify-between gap-3 rounded-md border px-3 py-2">
                <div className="flex min-w-0 items-center gap-2">
                  <FrameworkIcon
                    preset={values.preset}
                    className="size-5 shrink-0"
                  />
                  <span className="truncate text-sm">{frameworkLabel}</span>
                </div>
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  className="shrink-0"
                  aria-describedby="build-configuration-framework"
                  disabled={busy}
                  onClick={() => setFrameworkOpen(true)}
                >
                  Change
                </Button>
              </div>
            </div>

            <FormField
              control={form.control}
              name="directory"
              render={({ field }) => (
                <FormItem id="setting-root-directory" className="scroll-mt-4">
                  <FormLabel>Root directory</FormLabel>
                  <FormControl>
                    <Input
                      {...field}
                      onChange={(event) => {
                        field.onChange(event)
                        form.clearErrors('directory')
                      }}
                      readOnly={busy}
                      placeholder="./"
                      className="font-mono text-sm"
                    />
                  </FormControl>
                  {!directoryError && (
                    <FormDescription>
                      {rootDirectoryHelp(
                        null,
                        isUploadedSource,
                        project.main_branch
                      )}
                    </FormDescription>
                  )}
                  <FormMessage />
                </FormItem>
              )}
            />

            {/* An uploaded source has nothing to clone, so the choice would do nothing. */}
            {!isUploadedSource &&
              !isRepositoryRootDirectory(values.directory) && (
                <FormField
                  control={form.control}
                  name="pullOnlyRootDirectory"
                  render={({ field }) => (
                    <FormItem className="flex items-start gap-2 space-y-0">
                      <FormControl>
                        <Checkbox
                          className="mt-1"
                          checked={field.value}
                          disabled={busy}
                          onCheckedChange={(checked) =>
                            field.onChange(checked === true)
                          }
                        />
                      </FormControl>
                      <div className="space-y-1">
                        <FormLabel className="font-normal">
                          Pull only the root directory
                        </FormLabel>
                        <FormDescription>
                          Clone only this subdirectory instead of the whole
                          repository.
                        </FormDescription>
                      </div>
                    </FormItem>
                  )}
                />
              )}

            {isDockerfilePreset(values.preset) && (
              <>
                <FormField
                  control={form.control}
                  name="dockerfilePath"
                  render={({ field }) => (
                    <FormItem
                      id="setting-dockerfile-path"
                      className="scroll-mt-4"
                    >
                      <FormLabel>Dockerfile path</FormLabel>
                      <FormControl>
                        <Input
                          {...field}
                          readOnly={busy}
                          placeholder={DEFAULT_DOCKERFILE_PATH}
                          className="font-mono text-sm"
                        />
                      </FormControl>
                      <FormDescription>
                        Relative to the build context, which is the root
                        directory unless you set one below.
                      </FormDescription>
                      <FormMessage />
                    </FormItem>
                  )}
                />
                <FormField
                  control={form.control}
                  name="buildContext"
                  render={({ field }) => (
                    <FormItem
                      id="setting-build-context"
                      className="scroll-mt-4"
                    >
                      <FormLabel>Build context</FormLabel>
                      <FormControl>
                        <Input
                          {...field}
                          readOnly={busy}
                          placeholder={values.directory || './'}
                          className="font-mono text-sm"
                        />
                      </FormControl>
                      <FormDescription>
                        The directory sent to Docker, relative to the repository
                        root. Leave it blank to use the root directory; set it
                        when the Dockerfile needs files outside it.
                      </FormDescription>
                      <FormMessage />
                    </FormItem>
                  )}
                />
              </>
            )}

            {isComposePreset(values.preset) && (
              <FormField
                control={form.control}
                name="composePath"
                render={({ field }) => (
                  <FormItem id="setting-compose-file" className="scroll-mt-4">
                    <FormLabel>Compose file</FormLabel>
                    <FormControl>
                      <Input
                        {...field}
                        readOnly={busy}
                        placeholder={DEFAULT_COMPOSE_PATH}
                        className="font-mono text-sm"
                      />
                    </FormControl>
                    <FormDescription>
                      Relative to the root directory.
                    </FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />
            )}

            {rootError && (
              <p role="alert" className="text-sm text-destructive">
                {rootError}
              </p>
            )}
          </CardContent>
          <CardFooter>
            <p>Changes apply on the next deployment.</p>
            <Button type="submit" size="sm" disabled={!dirty || busy}>
              {busy && <Loader2 className="mr-2 size-4 animate-spin" />}
              Save
            </Button>
          </CardFooter>
        </Card>
      </form>

      <Dialog open={frameworkOpen} onOpenChange={setFrameworkOpen}>
        <DialogContent className="max-h-[85vh] max-w-5xl overflow-y-auto">
          <DialogHeader>
            <DialogTitle>Choose a framework</DialogTitle>
            <DialogDescription>
              Nothing changes until you save the build configuration.
            </DialogDescription>
          </DialogHeader>
          <FrameworkSelector
            presetData={
              detectedPresets ? { presets: [...detectedPresets] } : undefined
            }
            isLoading={isDetecting}
            isRefreshing={isRefreshingDetection}
            error={detectionError}
            selectedPreset={presetSelectionKey(values.preset, values.directory)}
            selectedDockerfilePath={
              isDockerfilePreset(values.preset)
                ? values.dockerfilePath
                : undefined
            }
            onSelectPreset={handleFrameworkSelection}
            onRefresh={onRefreshDetection}
          />
        </DialogContent>
      </Dialog>
    </Form>
  )
}
