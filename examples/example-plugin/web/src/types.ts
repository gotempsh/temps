// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export type IssueSeverity = "critical" | "warning" | "info";
export type ReportStatus = "running" | "completed" | "failed";

export interface SeoIssue {
  severity: IssueSeverity;
  code: string;
  message: string;
  recommendation: string;
}

export interface PageAnalysis {
  url: string;
  status_code: number;
  score: number;
  title: string | null;
  meta_description: string | null;
  canonical: string | null;
  h1_count: number;
  h2_count: number;
  image_count: number;
  images_without_alt: number;
  word_count: number;
  internal_links: number;
  external_links: number;
  has_og_title: boolean;
  has_og_description: boolean;
  has_og_image: boolean;
  has_robots_meta: boolean;
  has_viewport: boolean;
  has_charset: boolean;
  has_lang: boolean;
  load_time_ms: number;
  issues: SeoIssue[];
}

export interface ReportSummaryStats {
  pages_crawled: number;
  total_issues: number;
  critical: number;
  warnings: number;
  info: number;
  avg_page_score: number;
  missing_titles: number;
  missing_descriptions: number;
  missing_h1: number;
  images_without_alt: number;
  missing_canonical: number;
  missing_og_tags: number;
}

export interface SeoReport {
  id: string;
  url: string;
  score: number;
  pages: PageAnalysis[];
  summary: ReportSummaryStats;
  status: ReportStatus;
  created_at: string;
  completed_at: string | null;
  duration_ms: number;
}

export interface ReportSummary {
  id: string;
  url: string;
  score: number;
  pages_crawled: number;
  critical_issues: number;
  warning_issues: number;
  info_issues: number;
  status: ReportStatus;
  created_at: string;
  duration_ms: number;
}

export interface PluginSettings {
  default_max_pages: number;
  user_agent: string;
  request_timeout_secs: number;
  crawl_delay_ms: number;
}

export interface UpdateSettings {
  default_max_pages?: number;
  user_agent?: string;
  request_timeout_secs?: number;
  crawl_delay_ms?: number;
}
