// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export * from "./types";
export * from "./constants";
export * from "./utils";
export { EngagementTracker, type EngagementTrackerOptions, type EngagementData } from "./EngagementTracker";
export { SpeedTracker, type SpeedTrackerOptions } from "./SpeedTracker";
export { SessionRecorder, type SessionRecorderOptions } from "./SessionRecorder";
export {
  OUTBOUND_CLICK_EVENT,
  FILE_DOWNLOAD_EVENT,
  NOT_FOUND_EVENT,
  NOT_FOUND_MARKER_ATTRIBUTE,
  DEFAULT_FILE_DOWNLOAD_EXTENSIONS,
  NOT_FOUND_RECHECK_DELAY_MS,
  classifyLink,
  pathExtension,
  setupLinkTracking,
  notFoundEventData,
  hasNotFoundMarker,
  NotFoundMarkerWatcher,
  type AutoTrackOptions,
  type LinkClassification,
  type LinkLike,
  type ClassifyLinkOptions,
} from "./AutoTracking";
export {
  enableCookielessIdentity,
  isCookielessIdentity,
  onCookielessIdentity,
  onIdentityReset,
  clearStoredIdentity,
  getStoredVisitorId,
} from "./identity";
export {
  identifyVisitor,
  resetVisitor,
  ResetFailedError,
  IDENTIFY_ENDPOINT,
  RESET_ENDPOINT,
  type IdentifyVisitorOptions,
} from "./Identify";
export { createAnalytics } from "./Analytics";
export {
  FeatureFlags,
  exposureEventData,
  type FeatureFlagsOptions,
  type FlagContext,
  type FlagEvaluation,
  type FlagExposure,
  type FlagReason,
  type FlagsStatus,
} from "./FeatureFlags";
export {
  Surveys,
  pathMatches,
  validateAnswers,
  MAX_OPEN_TEXT_LENGTH,
  type Survey,
  type SurveyAnswer,
  type SurveyAnswers,
  type SurveyDisplay,
  type SurveyQuestion,
  type SurveyQuestionKind,
  type SurveysOptions,
  type SurveysStatus,
} from "./Surveys";
export { renderSurveyWidget, type SurveyWidgetHandlers } from "./SurveyWidget";
