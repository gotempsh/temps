// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { createContext } from 'react'

/**
 * A place in the conversation header for the open chat's own tools (its
 * activity, application, sandbox and working tabs). The header and the thread
 * are separate components; the thread owns the tools' state, the header owns
 * the space, so the thread renders into the header through this slot (see
 * `InAssistantHeader`) instead of adding a toolbar row above the messages.
 */
export const AssistantHeaderSlot = createContext<HTMLElement | null>(null)
