import { ProjectResponse } from '@/api/client'
import {
  createEnvironmentMutation,
  getEnvironmentsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Skeleton } from '@/components/ui/skeleton'
import { useMutation, useQuery } from '@tanstack/react-query'
import { ChevronRight, Layers } from 'lucide-react'
import { useState } from 'react'
import { Link, Route, Routes } from 'react-router-dom'
import { toast } from 'sonner'
import { CreateEnvironmentDialog } from './environments/CreateEnvironmentDialog'
import { EnvironmentDetail } from './environments/EnvironmentDetail'
import { cn } from '@/lib/utils'

interface EnvironmentsSettingsProps {
  project: ProjectResponse
}

function EnvironmentsList({ project }: EnvironmentsSettingsProps) {
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

  const handleCreateEnvironment = async ({
    name,
    branch,
  }: {
    name: string
    branch: string
  }) => {
    try {
      await createEnvironment.mutateAsync({
        path: {
          project_id: project.id,
        },
        body: {
          name,
          branch,
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
              />
            </EmptyPlaceholder>
          ) : (
            <div className="rounded-md border">
              <div className="divide-y">
                {environments?.map((env) => (
                  <Link
                    key={env.id}
                    to={`${env.id}`}
                    className="flex items-center justify-between p-4 hover:bg-muted/50 transition-colors"
                  >
                    <div className="space-y-1">
                      <p className="font-medium leading-none">{env.name}</p>
                      <p className="text-sm text-muted-foreground">
                        Branch: {env.branch}
                      </p>
                    </div>
                    <ChevronRight className="h-4 w-4 text-muted-foreground" />
                  </Link>
                ))}
              </div>
            </div>
          )}
        </div>
      </div>
    </div>
  )
}

export function EnvironmentsSettings({ project }: EnvironmentsSettingsProps) {
  return (
    <Routes>
      <Route index element={<EnvironmentsList project={project} />} />
      <Route
        path=":environmentId"
        element={<EnvironmentDetail project={project} />}
      />
    </Routes>
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
