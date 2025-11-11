import {
  listContainersOptions,
  getContainerDetailOptions,
  startContainerMutation,
  stopContainerMutation,
  restartContainerMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'

export function useContainers(projectId: string, environmentId: string) {
  return useQuery({
    ...listContainersOptions({
      path: {
        project_id: projectId,
        environment_id: environmentId,
      },
    }),
    staleTime: 5000,
  })
}

export function useContainer(
  projectId: string,
  environmentId: string,
  containerId: string
) {
  return useQuery({
    ...getContainerDetailOptions({
      path: {
        project_id: projectId,
        environment_id: environmentId,
        container_id: containerId,
      },
    }),
    staleTime: 5000,
  })
}

export function useContainerAction(projectId: string, environmentId: string) {
  const queryClient = useQueryClient()

  return useMutation({
    mutationFn: async ({
      containerId,
      action,
    }: {
      containerId: string
      action: 'start' | 'stop' | 'restart'
    }) => {
      const baseParams = {
        path: {
          project_id: projectId,
          environment_id: environmentId,
          container_id: containerId,
        },
      }

      if (action === 'start') {
        const { mutationFn } = startContainerMutation()
        return await mutationFn(baseParams)
      } else if (action === 'stop') {
        const { mutationFn } = stopContainerMutation()
        return await mutationFn(baseParams)
      } else if (action === 'restart') {
        const { mutationFn } = restartContainerMutation()
        return await mutationFn(baseParams)
      }
    },
    onSuccess: (_, { action, containerId }) => {
      // Invalidate the containers list
      queryClient.invalidateQueries({
        queryKey: listContainersOptions({
          path: {
            project_id: projectId,
            environment_id: environmentId,
          },
        }).queryKey,
      })

      // Invalidate the specific container detail
      queryClient.invalidateQueries({
        queryKey: getContainerDetailOptions({
          path: {
            project_id: projectId,
            environment_id: environmentId,
            container_id: containerId,
          },
        }).queryKey,
      })

      const actionLabel = action.charAt(0).toUpperCase() + action.slice(1)
      toast.success(`Container ${actionLabel.toLowerCase()}ed successfully`)
    },
    onError: (error: any, { action }) => {
      const actionLabel = action.charAt(0).toUpperCase() + action.slice(1)
      toast.error(
        `Failed to ${action} container: ${error?.message || 'Unknown error'}`
      )
    },
  })
}
