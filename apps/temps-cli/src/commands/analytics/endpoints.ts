// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps analytics` commands that map one to one onto an API operation, declared
 * as typed tables (see `src/lib/endpoint.ts`).
 */

import type { Command } from 'commander'
import {
  checkAnalyticsHasEvents,
  createTrafficFilter,
  deleteTrafficFilter,
  getActiveVisitors,
  getActivityStatus,
  getAnalyticsActiveVisitors,
  getAnalyticsEventsCount,
  getAnalyticsGoal,
  getAnalyticsIngestKeyEndpoint,
  getAnalyticsProjects,
  getAnalyticsSessionEvents,
  getAnalyticsVisitorSessions,
  getApiCallers,
  getApiRoutes,
  getDashboardProjectsAnalytics,
  getEventDetail,
  getEventEntries,
  getEventTypeBreakdown,
  getEventVisitors,
  getEventsTimeline,
  getFilteredTraffic,
  getGeneralStats,
  getGlobalAnalytics,
  getHourlyVisits,
  getLiveVisitorsList,
  getPageFlow,
  getPageHourlySessions,
  getPagePathDetail,
  getPagePathVisitors,
  getPagePathsSparklines,
  getProjectAnalyticsSettings,
  getPropertyTimeline,
  getRecentActivity,
  getSessionDetails,
  getSessionEvents,
  getSessionLogs,
  getVisitorByGuid,
  getVisitorById,
  getVisitorDetails,
  getVisitorFacets,
  getVisitorInfo,
  getVisitorJourney,
  getVisitorStats,
  getVisitors,
  hasAnalyticsEvents,
  hasPerformanceMetrics,
  identifyVisitorForToken,
  listTrafficFilters,
  previewActivityReport,
  recordConsoleEvent,
  runActivityReport,
  saveActivitySettings,
  suggestActivityGoals,
  updateProjectAnalyticsSettings,
} from '../../api/sdk.gen.js'
import { endpoint, topGroup } from '../../lib/endpoint.js'

export function registerAnalyticsEndpoints(program: Command): void {
  const group = topGroup(program, 'analytics', "analytics")

  endpoint(group, getAnalyticsActiveVisitors, {
    name: "get-active-visitors",
    description: "Get detailed active visitors",
    query: {
      project_id: { kind: 'int!', help: "Project ID" },
      environment_id: { kind: 'int', help: "Environment ID (optional)" },
      deployment_id: { kind: 'int', help: "Deployment ID (optional)" },
      window_minutes: { kind: 'int', help: "Time window in minutes for active visitors (default: 5)" },
    },
  })

  endpoint(group, getEventDetail, {
    name: "get-event-detail",
    description: "Get detailed analytics for a specific event",
    query: {
      event_name: { kind: 'string!', help: "Event name to get details for" },
      project_id: { kind: 'int!', help: "Project ID" },
      environment_id: { kind: 'int', help: "Environment ID (optional)" },
      start_date: { kind: 'string!', help: "Start date (ISO 8601)" },
      end_date: { kind: 'string!', help: "End date (ISO 8601)" },
      bucket_interval: { kind: 'string', help: "Bucket interval: hour, day, week, month (default: auto)" },
    },
  })

  endpoint(group, getEventEntries, {
    name: "get-event-entries",
    description: "Get paginated list of raw occurrences of a specific event, including custom JSON properties",
    query: {
      event_name: { kind: 'string!', help: "Event name to list occurrences for" },
      project_id: { kind: 'int!', help: "Project ID" },
      environment_id: { kind: 'int', help: "Environment ID (optional)" },
      start_date: { kind: 'string!', help: "Start date (ISO 8601)" },
      end_date: { kind: 'string!', help: "End date (ISO 8601)" },
      page: { kind: 'int', help: "Page number (1-based, default: 1)" },
      per_page: { kind: 'int', help: "Items per page (default: 20, max: 100)" },
    },
  })

  endpoint(group, getEventVisitors, {
    name: "get-event-visitors",
    description: "Get paginated list of visitors who triggered a specific event",
    query: {
      event_name: { kind: 'string!', help: "Event name to list visitors for" },
      project_id: { kind: 'int!', help: "Project ID" },
      environment_id: { kind: 'int', help: "Environment ID (optional)" },
      start_date: { kind: 'string!', help: "Start date (ISO 8601)" },
      end_date: { kind: 'string!', help: "End date (ISO 8601)" },
      page: { kind: 'int', help: "Page number (1-based, default: 1)" },
      per_page: { kind: 'int', help: "Items per page (default: 20, max: 100)" },
    },
  })

  endpoint(group, getAnalyticsEventsCount, {
    name: "get-events-count",
    description: "Get analytics events count",
    query: {
      start_date: { kind: 'string!', help: "Start date in format YYYY-MM-DD HH:MM:SS" },
      end_date: { kind: 'string!', help: "End date in format YYYY-MM-DD HH:MM:SS" },
      project_id: { kind: 'int!', help: "Project ID or slug" },
      limit: { kind: 'int', help: "Maximum number of results to return" },
      environment_id: { kind: 'int', help: "Environment ID (optional)" },
      custom_events_only: { kind: 'bool', help: "Only return custom events, excluding system events like page_view, page_leave, heartbeat (default: true)" },
      breakdown: { kind: 'string', help: "Breakdown by geography: 'country', 'region', or 'city' (optional)" },
    },
  })

  endpoint(group, getGeneralStats, {
    name: "get-stats",
    description: "Get general statistics across all projects for a time frame",
    query: {
      start_date: { kind: 'string!', help: "Start date in format YYYY-MM-DD HH:MM:SS" },
      end_date: { kind: 'string!', help: "End date in format YYYY-MM-DD HH:MM:SS" },
      project_ids: { kind: 'ints', help: "Optional: Filter by specific project IDs (comma-separated)" },
      environment_id: { kind: 'int', help: "Environment ID (optional)" },
      include_project_breakdown: { kind: 'bool', help: "Whether to include per-project breakdown (default: false)" },
    },
  })

  endpoint(group, getGlobalAnalytics, {
    name: "get-global",
    description: "Get global analytics",
    query: {
      facet: { kind: 'string', values: ["summary","traffic","pages","events","breakdown","speed"] },
      project_id: 'int',
      environment_id: 'int',
      start_date: 'string!',
      end_date: 'string!',
      search: 'string',
      dimension: 'string',
      device: 'string',
      sort_by: 'string',
      sort_order: 'string',
      page: 'int',
      per_page: 'int',
      min_views: 'int',
      min_sessions: 'int',
    },
  })

  endpoint(group, getAnalyticsProjects, {
    name: "get",
    description: "Get analytics projects",
  })

  endpoint(group, checkAnalyticsHasEvents, {
    name: "check-has-events",
    description: "Check analytics has events",
    query: {
      project_id: { kind: 'int!', help: "Project ID" },
      environment_id: { kind: 'int', help: "Environment ID (optional)" },
    },
  })

  endpoint(group, getLiveVisitorsList, {
    name: "get-live-visitors-list",
    description: "Get list of currently live visitors from visitor table",
    query: {
      project_id: { kind: 'int!', help: "Project ID" },
      environment_id: { kind: 'int', help: "Environment ID (optional)" },
      window_minutes: { kind: 'int', help: "Time window in minutes for live visitors (default: 5)" },
    },
  })

  endpoint(group, getPageFlow, {
    name: "get-page-flow",
    description: "Get page flow analytics: entry pages, exit pages, drop-off points, and page transitions",
    query: {
      project_id: { kind: 'int!', help: "Project ID" },
      environment_id: { kind: 'int', help: "Environment ID (optional)" },
      start_date: { kind: 'string!', help: "Start date in ISO 8601 format" },
      end_date: { kind: 'string!', help: "End date in ISO 8601 format" },
      limit: { kind: 'int', help: "Max entry/exit pages to return (default: 20, max: 100)" },
      transitions_limit: { kind: 'int', help: "Max page transitions to return (default: 50, max: 200)" },
      min_views_for_dropoff: { kind: 'int', help: "Minimum views for drop-off analysis (default: 5)" },
    },
  })

  endpoint(group, getPageHourlySessions, {
    name: "get-page-hourly-sessions",
    description: "Get page hourly sessions",
    query: {
      page_path: { kind: 'string!', help: "The page path to get sessions for" },
      project_id: { kind: 'int!', help: "Project ID" },
      environment_id: { kind: 'int', help: "Environment ID (optional)" },
      start_time: { kind: 'string!', help: "Start time in format YYYY-MM-DD HH:MM:SS" },
      end_time: { kind: 'string!', help: "End time in format YYYY-MM-DD HH:MM:SS" },
      bucket_interval: { kind: 'string', help: "Bucket interval: 'hour', 'day', 'week', or 'month' (default: auto-determined based on range)" },
    },
  })

  endpoint(group, getPagePathDetail, {
    name: "get-page-path-detail",
    description: "Get detailed analytics for a specific page path Returns visitors, page views, activity over time,..",
    query: {
      page_path: { kind: 'string!', help: "The page path to get details for (URL-encoded)" },
      project_id: { kind: 'int!', help: "Project ID" },
      environment_id: { kind: 'int', help: "Environment ID (optional)" },
      start_date: { kind: 'string!', help: "Start date in ISO 8601 format" },
      end_date: { kind: 'string!', help: "End date in ISO 8601 format" },
      bucket_interval: { kind: 'string', help: "Bucket interval for time series: 'hour', 'day', 'week', 'month' (default: auto based on date range)" },
    },
  })

  endpoint(group, getPagePathVisitors, {
    name: "get-page-path-visitors",
    description: "Get individual visitor sessions for a specific page path",
    query: {
      page_path: { kind: 'string!', help: "The page path to get visitors for (URL-encoded)" },
      project_id: { kind: 'int!', help: "Project ID" },
      environment_id: { kind: 'int', help: "Environment ID (optional)" },
      start_date: { kind: 'string!', help: "Start date in ISO 8601 format" },
      end_date: { kind: 'string!', help: "End date in ISO 8601 format" },
      page: { kind: 'int', help: "Page number (1-based, default: 1)" },
      per_page: { kind: 'int', help: "Items per page (default: 50, max: 100)" },
    },
  })

  endpoint(group, getPagePathsSparklines, {
    name: "get-page-paths-sparklines",
    description: "Get page paths sparklines",
    query: {
      project_id: { kind: 'int!', help: "Project ID" },
      environment_id: { kind: 'int', help: "Environment ID (optional)" },
      start_time: { kind: 'string!', help: "Start time in ISO 8601 format" },
      end_time: { kind: 'string!', help: "End time in ISO 8601 format" },
      page_paths: { kind: 'string!', help: "Comma-separated list of page paths" },
    },
  })

  endpoint(group, getRecentActivity, {
    name: "get-recent-activity",
    description: "Get recent activity events for real-time activity feed",
    query: {
      project_id: { kind: 'int!', help: "Project ID" },
      environment_id: { kind: 'int', help: "Environment ID (optional)" },
      since_id: { kind: 'int', help: "Return events with ID greater than this (cursor-based polling)" },
      limit: { kind: 'int', help: "Max events to return (default: 50, max: 100)" },
    },
  })

  endpoint(group, getSessionDetails, {
    name: "get-session-details",
    description: "Get detailed information about a specific session including events and request logs",
    path: { session_id: 'int' },
    query: {
      project_id: { kind: 'int!', help: "Project ID or slug" },
      environment_id: { kind: 'int', help: "Environment ID (optional)" },
    },
  })

  endpoint(group, getAnalyticsSessionEvents, {
    name: "get-session-events",
    description: "Get analytics session events",
    path: { session_id: 'int' },
    query: {
      project_id: { kind: 'int!', help: "Project ID or slug" },
      environment_id: { kind: 'int', help: "Environment ID (optional)" },
      start_date: { kind: 'string', help: "Start date in format YYYY-MM-DD HH:MM:SS" },
      end_date: { kind: 'string', help: "End date in format YYYY-MM-DD HH:MM:SS" },
      limit: { kind: 'int', help: "Number of results to return (default: 100)" },
      offset: { kind: 'int', help: "Number of results to skip (default: 0)" },
    },
  })

  endpoint(group, getSessionLogs, {
    name: "get-session-logs",
    description: "Get session logs",
    path: { session_id: 'int' },
    query: {
      project_id: { kind: 'int!', help: "Project ID or slug" },
      environment_id: { kind: 'int', help: "Environment ID (optional)" },
      start_date: { kind: 'string', help: "Start date in format YYYY-MM-DD HH:MM:SS" },
      end_date: { kind: 'string', help: "End date in format YYYY-MM-DD HH:MM:SS" },
      limit: { kind: 'int', help: "Number of results to return (default: 100)" },
      offset: { kind: 'int', help: "Number of results to skip (default: 0)" },
    },
  })

  endpoint(group, getVisitorFacets, {
    name: "get-visitor-facets",
    description: "Get filter dropdown contents for the visitors page",
    query: {
      start_date: { kind: 'string!', help: "Start date in format YYYY-MM-DD HH:MM:SS" },
      end_date: { kind: 'string!', help: "End date in format YYYY-MM-DD HH:MM:SS" },
      project_id: { kind: 'int!', help: "Project ID or slug" },
      environment_id: { kind: 'int', help: "Environment ID (optional)" },
      include_crawlers: { kind: 'bool', help: "Include crawlers (default: false)" },
      has_activity_only: { kind: 'bool', help: "Hide ghost visitors (default: true)" },
      per_facet_limit: { kind: 'int', help: "Top N values per dimension (default: 50, max: 200)" },
      filter_country: { kind: 'string', help: "Geolocation country" },
      filter_region: { kind: 'string', help: "Geolocation region" },
      filter_city: { kind: 'string', help: "Geolocation city" },
      filter_channel: { kind: 'string', help: "First-touch channel" },
      filter_referrer: { kind: 'string', help: "First-touch referrer hostname (use 'Direct' for null)" },
      filter_user_id: { kind: 'string', help: "User id set by identify(): the same user on every device" },
    },
  })

  endpoint(group, getVisitors, {
    name: "get-visitors",
    description: "Get list of visitors with summary information",
    query: {
      start_date: { kind: 'string!', help: "Start date in format YYYY-MM-DD HH:MM:SS" },
      end_date: { kind: 'string!', help: "End date in format YYYY-MM-DD HH:MM:SS" },
      project_id: { kind: 'int!', help: "Project ID or slug" },
      environment_id: { kind: 'int', help: "Environment ID (optional)" },
      include_crawlers: { kind: 'bool', help: "Include crawlers (default: false)" },
      limit: { kind: 'int', help: "Maximum number of visitors to return (default: 50)" },
      offset: { kind: 'int', help: "Number of visitors to skip (default: 0)" },
      has_activity_only: { kind: 'bool', help: "Filter to only include visitors with recorded activity (events/sessions)" },
      filter_country: { kind: 'string', help: "Geolocation country" },
      filter_region: { kind: 'string', help: "Geolocation region" },
      filter_city: { kind: 'string', help: "Geolocation city" },
      filter_channel: { kind: 'string', help: "First-touch channel" },
      filter_referrer: { kind: 'string', help: "First-touch referrer hostname (use 'Direct' for null)" },
      filter_user_id: { kind: 'string', help: "User id set by identify(): the same user on every device" },
    },
  })

  endpoint(group, getVisitorByGuid, {
    name: "get-visitor-by-guid",
    description: "Get visitor by GUID with geolocation data",
    path: { visitor_id: 'string' },
    query: {
      project_id: { kind: 'int!', help: "Project ID" },
      environment_id: { kind: 'int', help: "Environment ID (optional)" },
    },
  })

  endpoint(group, getVisitorById, {
    name: "get-visitor-by-id",
    description: "Get visitor by numeric ID with geolocation data",
    path: { id: 'int' },
    query: {
      project_id: { kind: 'int!', help: "Project ID" },
      environment_id: { kind: 'int', help: "Environment ID (optional)" },
    },
  })

  endpoint(group, getVisitorDetails, {
    name: "get-visitor-details",
    description: "Get detailed information about a specific visitor by numeric ID",
    path: { visitor_id: 'int' },
    query: {
      project_id: { kind: 'int!', help: "Project ID or slug" },
      environment_id: { kind: 'int', help: "Environment ID (optional)" },
    },
  })

  endpoint(group, identifyVisitorForToken, {
    name: "identify-visitor-for-token",
    description: "Identify a visitor of the token's own project (ADR 109 D4)",
    path: { visitor_id: 'string' },
    body: {
      traits: { kind: 'json', help: "Traits merged into the visitor's 'custom_data' at the top level; a key set to 'null' is removed" },
      user_id: { kind: 'string!', help: "The app's own id for the person (database id, UUID, email)" },
    },
  })

  endpoint(group, getVisitorInfo, {
    name: "get-visitor-info",
    description: "Get visitor record from database",
    path: { visitor_id: 'int' },
    query: {
      project_id: { kind: 'int!', help: "Project ID or slug" },
    },
  })

  endpoint(group, getVisitorJourney, {
    name: "get-visitor-journey",
    description: "Get the complete visitor journey: all events across all sessions, grouped by session",
    path: { visitor_id: 'int' },
    query: {
      project_id: { kind: 'int!', help: "Project ID" },
      limit_sessions: { kind: 'int', help: "Maximum number of sessions to return (default: 50)" },
    },
  })

  endpoint(group, getAnalyticsVisitorSessions, {
    name: "get-visitor-sessions",
    description: "Get all sessions for a specific visitor by numeric ID",
    path: { visitor_id: 'int' },
    query: {
      project_id: { kind: 'int!', help: "Project ID or slug" },
      environment_id: { kind: 'int', help: "Environment ID (optional)" },
      limit: { kind: 'int', help: "Maximum number of sessions to return (default: 100)" },
    },
  })

  endpoint(group, getVisitorStats, {
    name: "get-visitor-stats",
    description: "Get visitor statistics",
    path: { visitor_id: 'int' },
    query: {
      project_id: { kind: 'int!', help: "Project ID or slug" },
    },
  })

  endpoint(group, getDashboardProjectsAnalytics, {
    name: "events get-dashboard",
    description: "Get dashboard analytics for multiple projects in a single batch request",
    query: {
      project_ids: { kind: 'string!', help: "Comma-separated list of project IDs" },
      start_date: { kind: 'string!', help: "Start date for filtering" },
      end_date: { kind: 'string!', help: "End date for filtering" },
    },
  })

  endpoint(group, hasPerformanceMetrics, {
    name: "performance has-metrics",
    description: "Check if performance metrics exist for a project",
    query: {
      project_id: { kind: 'int!', help: "Project ID" },
    },
  })

  endpoint(group, getActiveVisitors, {
    name: "events get-active-visitors",
    description: "Get active visitors count",
    path: { project_id: 'project' },
    query: {
      environment_id: { kind: 'int', help: "Filter by environment ID" },
      deployment_id: { kind: 'int', help: "Filter by deployment ID" },
    },
  })

  endpoint(group, getActivityStatus, {
    name: "visitors get-activity-status",
    description: "Get activity status",
    path: { project_id: 'project' },
    query: {
      environment_id: 'int',
    },
  })

  endpoint(group, saveActivitySettings, {
    name: "visitors save-activity-settings",
    description: "Save activity settings",
    path: { project_id: 'project' },
    body: {
      application_context: 'string!',
      categories: 'json!',
      daily_enabled: 'bool!',
      environment_id: 'int',
      goal_title: { kind: 'string', help: "Short operator-facing label for the selected analysis goal" },
      min_page_paths: 'int',
      min_sessions: 'int',
      property_keys: { kind: 'strings!', help: "Only these event-property keys may be sent to the provider" },
      share_activity_with_ai: { kind: 'bool!', help: "Explicit permission to send the selected analytics fields to the configured AI provider" },
      source_domain: 'string',
      source_url: 'string',
    },
  })

  endpoint(group, suggestActivityGoals, {
    name: "visitors suggest-activity-goals",
    description: "Suggest activity goals",
    path: { project_id: 'project' },
    body: {
      environment_id: 'int',
      share_with_ai: 'bool!',
      url: 'string!',
    },
  })

  endpoint(group, previewActivityReport, {
    name: "visitors preview-activity-report",
    description: "Preview activity report",
    path: { project_id: 'project' },
    body: {
      environment_id: 'int',
      goal: 'string!',
      min_page_paths: 'int',
      min_sessions: 'int',
      property_keys: 'strings',
      share_activity_with_ai: 'bool!',
      source_domain: 'string',
      source_url: 'string',
    },
  })

  endpoint(group, runActivityReport, {
    name: "visitors run-activity-report",
    description: "Run activity report",
    path: { project_id: 'project' },
  })

  endpoint(group, getFilteredTraffic, {
    name: "settings get-filtered-traffic",
    description: "How many events were counted and how many were kept out of the project's analytics, by reason, over the..",
    path: { project_id: 'project' },
    query: {
      days: { kind: 'int', help: "Window in days, ending now (default 7, max 30)" },
    },
  })

  endpoint(group, getAnalyticsGoal, {
    name: "conversions get-goal",
    description: "Get one conversion goal",
    path: { project_id: 'project', goal_id: 'int' },
  })

  endpoint(group, getAnalyticsIngestKeyEndpoint, {
    name: "keys get-ingest-endpoint",
    description: "The public address an app Temps does not deploy should send analytics to",
    path: { project_id: 'project' },
  })

  endpoint(group, getProjectAnalyticsSettings, {
    name: "settings get",
    description: "Get a project's analytics traffic settings and whether datacenter detection is available on this instance",
    path: { project_id: 'project' },
  })

  endpoint(group, updateProjectAnalyticsSettings, {
    name: "settings update",
    description: "Change a project's analytics traffic settings",
    path: { project_id: 'project' },
    body: {
      exclude_datacenter_traffic: 'bool',
      exclude_spam_referrers: 'bool',
    },
  })

  endpoint(group, listTrafficFilters, {
    name: "settings list-traffic-filters",
    description: "List a project's traffic filters, newest first",
    path: { project_id: 'project' },
    query: {
      kind: { kind: 'string', help: "Only filters of this kind", values: ["referrer","ip","asn","country"] },
      page: { kind: 'int', help: "Page number, starting at 1 (default 1)" },
      page_size: { kind: 'int', help: "Page size (default 20, max 100)" },
    },
  })

  endpoint(group, createTrafficFilter, {
    name: "settings create-traffic-filter",
    description: "Add a traffic filter",
    path: { project_id: 'project' },
    body: {
      kind: { kind: 'string!', help: "What a traffic filter matches on", values: ["referrer","ip","asn","country"] },
      note: { kind: 'string', help: "Why the filter exists, for whoever reviews it later" },
      value: { kind: 'string!', help: "A domain or URL ('referrer'), an IP address or CIDR range ('ip'), a network number with or without the..." },
    },
  })

  endpoint(group, deleteTrafficFilter, {
    name: "settings delete-traffic-filter",
    description: "Remove a traffic filter",
    path: { project_id: 'project', filter_id: 'int' },
    confirm: "This will remove a traffic filter.",
  })

  endpoint(group, getApiCallers, {
    name: "get-api-callers",
    description: "Return the top callers (by client IP, ranked by request count) in a project's API traffic window",
    path: { project_id: 'project' },
    query: {
      environment_id: { kind: 'int', help: "Environment ID (optional)" },
      start_date: { kind: 'string!', help: "Window start (ISO 8601)" },
      end_date: { kind: 'string!', help: "Window end (ISO 8601)" },
      limit: { kind: 'int', help: "Max callers to return (default: 20, max: 100)" },
      offset: { kind: 'int', help: "Ranked callers to skip (default: 0)" },
    },
  })

  endpoint(group, getApiRoutes, {
    name: "get-api-routes",
    description: "Return the top routes (by request count) in a project's API traffic, grouped by raw '(method, path)'",
    path: { project_id: 'project' },
    query: {
      environment_id: { kind: 'int', help: "Environment ID (optional)" },
      start_date: { kind: 'string!', help: "Window start (ISO 8601)" },
      end_date: { kind: 'string!', help: "Window end (ISO 8601)" },
      limit: { kind: 'int', help: "Max routes to return (default: 20, max: 100)" },
      offset: { kind: 'int', help: "Ranked routes to skip (default: 0)" },
    },
  })

  endpoint(group, getEventTypeBreakdown, {
    name: "events get-type-breakdown",
    description: "Get event type breakdown",
    path: { project_id: 'project' },
    query: {
      start_date: { kind: 'string!', help: "Start date for filtering events" },
      end_date: { kind: 'string!', help: "End date for filtering events" },
      environment_id: { kind: 'int', help: "Filter by environment ID" },
      aggregation_level: { kind: 'string', help: "Aggregation level: events, sessions, or visitors (default: events)" },
    },
  })

  endpoint(group, recordConsoleEvent, {
    name: "events record-console",
    description: "Record an analytics event via the console API with explicit project ID",
    path: { project_id: 'project' },
    body: {
      deployment_id: { kind: 'int!', help: "Deployment ID to attribute the event to" },
      environment_id: { kind: 'int!', help: "Environment ID to attribute the event to" },
      event_data: { kind: 'json', help: "Arbitrary JSON event data" },
      event_name: { kind: 'string!', help: "Event name (e.g. \"purchase\", \"signup\", custom event names)" },
      request_path: { kind: 'string', help: "Page path context (defaults to \"/\")" },
      request_query: { kind: 'string', help: "Query string context" },
      session_id: { kind: 'string', help: "Encrypted '_temps_sid' cookie value from the user's browser" },
      visitor_id: { kind: 'string', help: "Encrypted '_temps_visitor_id' cookie value from the user's browser" },
    },
  })

  endpoint(group, getPropertyTimeline, {
    name: "events get-property-timeline",
    description: "Get property timeline by grouping events by a column over time",
    path: { project_id: 'project' },
    query: {
      start_date: { kind: 'string!', help: "Start date in '%Y-%m-%d %H:%M:%S' format" },
      end_date: { kind: 'string!', help: "End date in '%Y-%m-%d %H:%M:%S' format" },
      group_by: { kind: 'string!', help: "Column to group by (channel, device_type, browser, etc.)" },
      environment_id: { kind: 'int', help: "Filter by environment ID" },
      deployment_id: { kind: 'int', help: "Filter by deployment ID" },
      event_name: { kind: 'string', help: "Filter by event name" },
      aggregation_level: { kind: 'string', help: "Aggregation level: events, sessions, or visitors - default: events" },
      bucket_size: { kind: 'string', help: "Time bucket: hour, day, week, month (default: auto-detect)" },
      include_crawlers: { kind: 'bool', help: "Include crawler/bot traffic (default: false)" },
    },
  })

  endpoint(group, getEventsTimeline, {
    name: "events get-timeline",
    description: "Get events timeline",
    path: { project_id: 'project' },
    query: {
      start_date: { kind: 'string!', help: "Start date for filtering events" },
      end_date: { kind: 'string!', help: "End date for filtering events" },
      environment_id: { kind: 'int', help: "Filter by environment ID" },
      event_name: { kind: 'string', help: "Filter by specific event name" },
      bucket_size: { kind: 'string', help: "Bucket size: hour, day, or week (auto-detected if not specified)" },
      aggregation_level: { kind: 'string', help: "Aggregation level: events, sessions, or visitors (default: events)" },
    },
  })

  endpoint(group, hasAnalyticsEvents, {
    name: "events has",
    description: "Check if project has any analytics events",
    path: { project_id: 'project' },
  })

  endpoint(group, getHourlyVisits, {
    name: "events get-hourly-visits",
    description: "Get hourly visits",
    path: { project_id: 'project' },
    query: {
      start_date: { kind: 'string!', help: "Start date for filtering visits" },
      end_date: { kind: 'string!', help: "End date for filtering visits" },
      environment_id: { kind: 'int', help: "Filter by environment ID" },
      aggregation_level: { kind: 'string', help: "Aggregation level: events (page views), sessions (unique sessions), or visitors (unique visitors) -..." },
    },
  })

  endpoint(group, getSessionEvents, {
    name: "events get-session",
    description: "Get events for a specific session",
    path: { session_id: 'string' },
    query: {
      environment_id: { kind: 'int', help: "Filter by environment ID" },
    },
  })
}
