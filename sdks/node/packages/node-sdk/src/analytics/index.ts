// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { identify } from './identify';

export {
  identify,
  visitorIdFromRequest,
  VISITOR_COOKIE,
  type IdentifyOptions,
  type IdentifyResult,
  type IdentifyTraitValue,
  type RequestLike,
} from './identify';

/** Server-side analytics helpers: `analytics.identify(request, { userId })`. */
export const analytics = { identify };
