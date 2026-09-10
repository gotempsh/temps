// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Plugin settings (matches Rust PluginSettings) */
export interface PluginSettings {
  apiKey: string | null;
  searchEngine: string;
  autoSubmit: boolean;
  maxPages: number;
  resubmitAfterHours: number;
  userAgent: string;
}

/** Partial settings update */
export interface UpdateSettings {
  apiKey?: string;
  searchEngine?: string;
  autoSubmit?: boolean;
  maxPages?: number;
  resubmitAfterHours?: number;
  userAgent?: string;
}

/** Submission record summary */
export interface SubmissionResponse {
  url: string;
  host: string;
  lastSubmittedAt: string;
  lastModifiedAt: string | null;
  submissionCount: number;
  deploymentId: number | null;
  projectId: number | null;
}

/** A page suggestion for (re)submission */
export interface PageSuggestion {
  url: string;
  host: string;
  reason: SuggestionReason;
  lastSubmittedAt: string | null;
  lastModifiedAt: string | null;
  currentLastModified: string | null;
  contentChanged: boolean;
}

export type SuggestionReason =
  | "never_submitted"
  | "stale_submission"
  | "content_modified"
  | "content_hash_changed"
  | "new_page";

/** Result of a manual submit */
export interface SubmissionResult {
  submittedCount: number;
  skippedCount: number;
  failedCount: number;
  apiStatus: number | null;
  error: string | null;
  submittedUrls: string[];
}

/** Response from the suggestions endpoint */
export interface SuggestionsResponse {
  suggestions: PageSuggestion[];
  totalPagesChecked: number;
  pagesNeedingSubmission: number;
}
