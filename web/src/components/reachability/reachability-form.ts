// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// The *Make it reachable* page's form (ADR 069 D4). Who can open it and the
// sign-in gate live in the URL (ADR 069 D2); this holds what the user types
// and picks below them.

import { z } from 'zod'

export const reachabilityFormSchema = z.object({
  address: z.string().trim(),
  /** For `tailnet_users`: logins and tags, as typed (ADR 065 D6). */
  principals: z.string(),
  option: z.string().min(1, 'Pick a way in.'),
  values: z.record(z.string(), z.string()),
  /** The user accepted the option's `confirmation`. */
  confirmed: z.boolean(),
})

export type ReachabilityFormValues = z.infer<typeof reachabilityFormSchema>
