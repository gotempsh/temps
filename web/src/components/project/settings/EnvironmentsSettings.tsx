import { ProjectResponse } from '@/api/client'
import {
  createEnvironmentMutation,
  getEnvironmentsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Skeleton } from '@/components/ui/skeleton'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { useMutation, useQuery } from '@tanstack/react-query'
import { Layers } from 'lucide-react'
import { useState } from 'react'
import { useSearchParams } from 'react-router-dom'
import { toast } from 'sonner'
import { CreateEnvironmentDialog } from './environments/CreateEnvironmentDialog'
import { EnvironmentDetail } from './environments/EnvironmentDetail'
import { cn } from '@/lib/utils'

interface EnvironmentsSettingsProps {
  project: ProjectResponse
}

export function EnvironmentsSettings({ project }: EnvironmentsSettingsProps) {
  const [searchParams, setSearchParams] = useSearchParams()

  const {
    data: environments,
    refetch,
    isLoading,
  } = useQuery({
    ...getEnvironmentsOptions({
      path: {
        project_id: project.id,
      },
    }),
  })

  const createEnvironment = useMutation({
    ...createEnvironmentMutation(),
    meta: {
      errorTitle: 'Failed to create environment',
    },
  })

  const [open, setOpen] = useState(false)

  // Get initial selected tab from query param or first environment
  const envIdParam = searchParams.get('env')
  const initialValue = envIdParam ?? environments?.[0]?.id.toString()

  // Handle tab change - update query param
  const handleTabChange = (value: string) => {
    setSearchParams({ env: value })
  }

  const handleCreateEnvironment = async ({
    name,
    branch,
    isPreview,
  }: {
    name: string
    branch: string
    isPreview: boolean
  }) => {
    try {
      await createEnvironment.mutateAsync({
        path: {
          project_id: project.id,
        },
        body: {
          name,
          branch,
          is_preview: isPreview,
        },
      })

      refetch()
      toast.success('Environment created successfully')
    } catch (error) {
      toast.error('Failed to create environment')
      throw error
    }
  }

  if (isLoading) {
    return (
      <div className="space-y-6">
        <div className="flex flex-row items-center justify-between mb-6">
          <div className="space-y-1.5">
            <Skeleton className="h-8 w-[200px]" />
            <Skeleton className="h-5 w-[350px]" />
          </div>
          <Skeleton className="h-10 w-[140px]" />
        </div>
        <div className="space-y-2">
          {[...Array(3)].map((_, i) => (
            <Skeleton key={i} className="h-16 w-full" />
          ))}
        </div>
      </div>
    )
  }

  const hasEnvironments = (environments?.length ?? 0) > 0

  // Show the list view with tabs
  return (
    <div className="space-y-6">
      <div>
        <div className="flex flex-row items-center justify-between mb-6">
          <div className="space-y-1.5">
            <h2 className="text-2xl font-semibold tracking-tight">
              Environments
            </h2>
            <p className="text-sm text-muted-foreground">
              Manage deployment environments for your project.
            </p>
          </div>
          {hasEnvironments && (
            <CreateEnvironmentDialog
              onSubmit={handleCreateEnvironment}
              open={open}
              onOpenChange={setOpen}
              project={project}
            />
          )}
        </div>

        <div className="mt-6">
          {!hasEnvironments ? (
            <EmptyPlaceholder>
              <EmptyPlaceholder.Icon>
                <Layers className="h-6 w-6" />
              </EmptyPlaceholder.Icon>
              <EmptyPlaceholder.Title>No environments</EmptyPlaceholder.Title>
              <EmptyPlaceholder.Description>
                Create environments to manage different deployment stages for
                your project.
              </EmptyPlaceholder.Description>
              <CreateEnvironmentDialog
                onSubmit={handleCreateEnvironment}
                open={open}
                onOpenChange={setOpen}
                project={project}
              />
            </EmptyPlaceholder>
          ) : (
            <Tabs
              value={initialValue}
              onValueChange={handleTabChange}
              className="w-full"
            >
              <TabsList className="w-full justify-start border-b rounded-none h-auto p-0 bg-transparent">
                {environments?.map((env) => (
                  <TabsTrigger
                    key={env.id}
                    value={env.id.toString()}
                    className="data-[state=active]:border-b-2 data-[state=active]:border-primary rounded-none px-4 py-3"
                  >
                    <div className="flex flex-col items-start gap-1">
                      <span className="font-medium">{env.name}</span>
                      <span className="text-xs text-muted-foreground">
                        {env.branch}
                      </span>
                    </div>
                  </TabsTrigger>
                ))}
              </TabsList>

              {environments?.map((env) => (
                <TabsContent
                  key={env.id}
                  value={env.id.toString()}
                  className="mt-6"
                >
                  <EnvironmentDetail project={project} environmentId={env.id} />
                </TabsContent>
              ))}
            </Tabs>
          )}
        </div>
      </div>
    </div>
  )
}

// Add EmptyPlaceholder component (same as in EnvironmentVariablesSettings)
interface EmptyPlaceholderProps extends React.HTMLAttributes<HTMLDivElement> {}

function EmptyPlaceholder({
  className,
  children,
  ...props
}: EmptyPlaceholderProps) {
  return (
    <div
      className={cn(
        'flex min-h-[400px] flex-col items-center justify-center rounded-md border border-dashed p-8 text-center animate-in fade-in-50',
        className
      )}
      {...props}
    >
      <div className="mx-auto flex max-w-[420px] flex-col items-center justify-center text-center">
        {children}
      </div>
    </div>
  )
}

EmptyPlaceholder.Icon = function EmptyPlaceholderIcon({
  className,
  children,
  ...props
}: React.HTMLAttributes<HTMLDivElement>) {
  return (
    <div
      className={cn(
        'flex h-20 w-20 items-center justify-center rounded-full bg-muted',
        className
      )}
      {...props}
    >
      {children}
    </div>
  )
}

EmptyPlaceholder.Title = function EmptyPlaceholderTitle({
  className,
  children,
  ...props
}: React.HTMLAttributes<HTMLHeadingElement>) {
  return (
    <h2 className={cn('mt-6 text-xl font-semibold', className)} {...props}>
      {children}
    </h2>
  )
}

EmptyPlaceholder.Description = function EmptyPlaceholderDescription({
  className,
  children,
  ...props
}: React.HTMLAttributes<HTMLParagraphElement>) {
  return (
    <p
      className={cn(
        'mb-8 mt-2 text-center text-sm font-normal leading-6 text-muted-foreground',
        className
      )}
      {...props}
    >
      {children}
    </p>
  )
}
