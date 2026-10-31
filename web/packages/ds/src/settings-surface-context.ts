// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { createContext, useContext } from 'react'

/** True below a settings surface: a section there renders flat, not boxed. */
export const SettingsSurfaceContext = createContext(false)

/** Whether this point sits on a settings surface, where cards render flat. */
export function useOnSettingsSurface(): boolean {
  return useContext(SettingsSurfaceContext)
}
