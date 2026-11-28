// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { z } from 'zod'

/**
 * Email field for signing in to, or recovering, an account that already
 * exists. Deliberately lenient: the server decides whether the account
 * exists, so the form only catches obvious typos. `z.email()` requires a dot
 * in the domain and so refused `admin@localhost`, the administrator that
 * `temps start` creates for a local install.
 */
export const signInEmailSchema = z
  .string()
  .trim()
  .min(1, 'Enter your email address')
  .regex(/^[^\s@]+@[^\s@]+$/, 'Please enter a valid email address')
