// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export type ServiceCreationSuccessMessage<T> =
  string | ((createdService: T) => string)

interface CompleteServiceCreationOptions<T> {
  createdService: T
  onSuccess: (createdService: T) => void
  notifySuccess: (message: string) => void
  successMessage?: ServiceCreationSuccessMessage<T>
}

const DEFAULT_SUCCESS_MESSAGE = 'Service created successfully'

/**
 * Completes service creation through one notification path.
 *
 * Embedding workflows provide their preferred message and keep their
 * `onSuccess` callback focused on state updates, avoiding duplicate toasts.
 */
export function completeServiceCreation<T>({
  createdService,
  onSuccess,
  notifySuccess,
  successMessage = DEFAULT_SUCCESS_MESSAGE,
}: CompleteServiceCreationOptions<T>): void {
  const message =
    typeof successMessage === 'function'
      ? successMessage(createdService)
      : successMessage

  notifySuccess(message)
  onSuccess(createdService)
}
