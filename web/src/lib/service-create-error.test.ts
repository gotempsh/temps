// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, describe, expect, mock, spyOn, test } from 'bun:test'
import { toast } from 'sonner'
import { handleServiceCreateError } from './service-create-error'

afterEach(() => {
  spyOn(toast, 'error').mockRestore()
})

const context = { meta: { errorTitle: 'Failed to create service' } }

describe('handleServiceCreateError', () => {
  test('announces a refusal that is not a name conflict, with its reason', () => {
    const error = spyOn(toast, 'error').mockImplementation(() => 'id')
    const onNameConflict = mock(() => {})
    handleServiceCreateError(
      {
        title: 'Cluster Placement Not Reachable',
        detail:
          "Cluster 'pg-a' has members on both the control plane and worker nodes",
        error_code: 'CONFLICT',
      },
      context,
      onNameConflict
    )
    expect(error).toHaveBeenCalledWith('Failed to create service', {
      description:
        "Cluster 'pg-a' has members on both the control plane and worker nodes",
    })
    expect(onNameConflict).not.toHaveBeenCalled()
  })

  test('puts a taken name on the name field instead of a toast', () => {
    const error = spyOn(toast, 'error').mockImplementation(() => 'id')
    const onNameConflict = mock((_message: string) => {})
    handleServiceCreateError(
      {
        title: 'Conflict',
        detail: "A service named 'pg-a' already exists",
        error_code: 'SERVICE_NAME_CONFLICT',
      },
      context,
      onNameConflict
    )
    expect(onNameConflict).toHaveBeenCalledWith(
      "A service named 'pg-a' already exists"
    )
    expect(error).not.toHaveBeenCalled()
  })

  test('still announces something when the error has no problem body', () => {
    const error = spyOn(toast, 'error').mockImplementation(() => 'id')
    handleServiceCreateError({}, context, () => {})
    expect(error).toHaveBeenCalledWith('Failed to create service', undefined)
  })
})
