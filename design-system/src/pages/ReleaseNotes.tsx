// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Article, PageContainer, PageHeader, fmtDate } from '@temps-sdk/ds'

/** Reference screen for `Article`: long-form content read top to bottom, not scanned for facts. */
export default function ReleaseNotes() {
  return (
    <PageContainer>
      <PageHeader
        title="v0.14.0 — retry-aware webhooks"
        description={`Published ${fmtDate('2026-09-15T09:00:00Z')}`}
      />
      <Article>
        <p>
          Webhook deliveries now retry with backoff instead of failing
          silently after the first attempt. This closes the gap where a
          receiving endpoint's brief outage meant a deploy notification was
          gone for good.
        </p>
        <h2>What changed</h2>
        <ul>
          <li>
            Failed deliveries retry up to 5 times with exponential backoff
            (1s, 4s, 16s, 64s, 256s) before being marked <code>failed</code>.
          </li>
          <li>
            The delivery detail page now shows every attempt, not just the
            most recent one.
          </li>
          <li>
            A new <code>webhook.delivery.exhausted</code> event fires after
            the final retry, so you can alert on it separately from a plain
            failure.
          </li>
        </ul>
        <blockquote>
          Retries respect the endpoint's own rate limits — a 429 response
          extends the backoff instead of counting as a normal failure.
        </blockquote>
        <h2>Upgrading</h2>
        <p>
          No action needed. Existing webhook configurations pick up retry
          behavior automatically on the next delivery attempt.
        </p>
      </Article>
    </PageContainer>
  )
}
