import type { AnalyticsOptions, SessionRecordingConfig } from "@temps-sdk/analytics-core";
import { init } from "./index";

/**
 * Auto-init entrypoint. Reads configuration from the currently-executing
 * <script> tag's `data-*` attributes and boots Temps analytics on
 * DOMContentLoaded.
 *
 * Usage:
 *   <script
 *     defer
 *     src="https://cdn.jsdelivr.net/npm/@temps-sdk/analytics-browser@0.0.1/dist/temps.min.js">
 *   </script>
 */

function readDataset(): AnalyticsOptions {
  if (typeof document === "undefined") return {};
  const script = (document.currentScript as HTMLScriptElement | null)
    ?? document.querySelector<HTMLScriptElement>("script[data-temps]")
    ?? Array.from(document.scripts).reverse().find((s) => !!s.dataset?.domain || !!s.dataset?.basePath)
    ?? null;
  if (!script) return {};

  const ds = script.dataset;
  const opts: AnalyticsOptions = {};
  const recording: SessionRecordingConfig = {};

  if (ds.basePath) opts.basePath = ds.basePath;
  if (ds.domain) opts.domain = ds.domain;
  if (ds.disabled) opts.disabled = ds.disabled === "true";
  if (ds.ignoreLocalhost) opts.ignoreLocalhost = ds.ignoreLocalhost === "true";
  if (ds.autoTrackPageviews) opts.autoTrackPageviews = ds.autoTrackPageviews === "true";
  if (ds.autoTrackPageLeave) opts.autoTrackPageLeave = ds.autoTrackPageLeave === "true";
  if (ds.pageLeaveEventName) opts.pageLeaveEventName = ds.pageLeaveEventName;
  if (ds.autoTrackSpeedAnalytics) opts.autoTrackSpeedAnalytics = ds.autoTrackSpeedAnalytics === "true";
  if (ds.autoTrackEngagement) opts.autoTrackEngagement = ds.autoTrackEngagement === "true";
  if (ds.heartbeatInterval) opts.heartbeatInterval = Number(ds.heartbeatInterval);
  if (ds.inactivityTimeout) opts.inactivityTimeout = Number(ds.inactivityTimeout);
  if (ds.engagementThreshold) opts.engagementThreshold = Number(ds.engagementThreshold);
  if (ds.sessionRecording) opts.enableSessionRecording = ds.sessionRecording === "true";
  if (ds.sessionSampleRate) recording.sessionSampleRate = Number(ds.sessionSampleRate);
  if (ds.maskAllInputs) recording.maskAllInputs = ds.maskAllInputs === "true";
  if (ds.excludedPaths) recording.excludedPaths = ds.excludedPaths.split(",").map((s) => s.trim());
  if (Object.keys(recording).length > 0) opts.sessionRecordingConfig = recording;

  return opts;
}

function boot(): void {
  if (typeof window === "undefined") return;
  init(readDataset());
}

if (typeof document !== "undefined" && document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", boot, { once: true });
} else {
  boot();
}
