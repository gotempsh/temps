"use client";
import { useEffect, useRef, useCallback, useState, useMemo } from "react";
import { record, type eventWithTime } from "rrweb";
import { pack } from "@rrweb/packer";

export const SESSION_RECORDER_ENDPOINT = "session-replay";

interface SessionRecorderProps {
  basePath: string;  // Required, no default
  domain?: string;
  enabled?: boolean;
  excludedPaths?: string[];
  sessionSampleRate?: number;
  maskAllInputs?: boolean;
  maskTextSelector?: string;
  blockClass?: string;
  ignoreClass?: string;
  maskTextClass?: string;
  recordCanvas?: boolean;
  collectFonts?: boolean;
  slimDOMOptions?: Record<string, boolean>;
  maskInputOptions?: {
    password?: boolean;
    email?: boolean;
  };
  /**
   * Number of events to batch before sending. Default: 500
   * Events are sent when EITHER batchSize is reached OR flushInterval elapses (whichever comes first).
   * Increased from 200 to reduce request frequency.
   */
  batchSize?: number;
  /**
   * Interval in milliseconds to flush events. Default: 60000 (60s)
   * Events are sent when EITHER flushInterval elapses OR batchSize is reached (whichever comes first).
   * Increased from 30s to reduce request frequency.
   */
  flushInterval?: number;
  ignoreSelector?: string;
  blockSelector?: string;
  sampling?: {
    scroll?: number;
    media?: number;
    mouseInteraction?: boolean | {
      click?: boolean;
      dblclick?: boolean;
      contextmenu?: boolean;
      focus?: boolean;
      blur?: boolean;
      touchstart?: boolean;
      touchend?: boolean;
      touchcancel?: boolean;
      play?: boolean;
      pause?: boolean;
    };
    mousemove?: boolean | number;
    input?: "all" | "last";
    canvas?: number | "all";
  };
}

function generateSessionId(): string {
  if (typeof crypto !== "undefined" && crypto.randomUUID) {
    return crypto.randomUUID();
  }
  return `session_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`;
}

function generateVisitorId(): string {
  // Try to get a persistent visitor ID from localStorage
  if (typeof localStorage !== "undefined") {
    let visitorId = localStorage.getItem("temps_visitor_id");
    if (!visitorId) {
      visitorId = `visitor_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`;
      localStorage.setItem("temps_visitor_id", visitorId);
    }
    return visitorId;
  }
  return `visitor_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`;
}

function getSessionMetadata(_domain?: string): Record<string, unknown> {
  if (typeof window === "undefined") return {};

  const screen = window.screen || {};
  const navigator = window.navigator || {};

  return {
    visitorId: generateVisitorId(),
    userAgent: navigator.userAgent,
    language: navigator.language,
    timezone: Intl.DateTimeFormat().resolvedOptions().timeZone,
    screenWidth: screen.width,
    screenHeight: screen.height,
    colorDepth: screen.colorDepth || 24,
    viewportWidth: window.innerWidth,
    viewportHeight: window.innerHeight,
    url: window.location.href,
    timestamp: new Date().toISOString(),
  };
}

export function SessionRecorder({
  basePath,  // Required, no default
  domain,
  enabled = false,
  excludedPaths = [],
  sessionSampleRate = 1.0,
  maskAllInputs = true,
  maskTextSelector = "[data-mask]",
  blockClass = "rr-block",
  ignoreClass = "rr-ignore",
  maskTextClass = "rr-mask",
  ignoreSelector = "[data-ignore]",
  blockSelector = "[data-private]",
  recordCanvas = false,
  collectFonts = true,
  slimDOMOptions = {
    script: false,
    comment: true,
    headFavicon: true,
    headWhitespace: true,
    headMetaDescKeywords: true,
    headMetaSocial: true,
    headMetaRobots: true,
    headMetaHttpEquiv: true,
    headMetaAuthorship: true,
    headMetaVerification: true,
  },
  maskInputOptions = {
    password: true,
    email: true,
  },
  batchSize = 100,
  flushInterval = 10000,
  sampling = {},
}: SessionRecorderProps): null {
  const stopFnRef = useRef<(() => void) | null>(null);
  const eventsRef = useRef<eventWithTime[]>([]);
  const sessionIdRef = useRef<string>("");
  const sessionInitializedRef = useRef<boolean>(false);
  const flushTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const initRetryCountRef = useRef<number>(0);
  const initFailedRef = useRef<boolean>(false); // Track if init has permanently failed
  const takeSnapshotRef = useRef<(() => void) | null>(null); // Function to force a full snapshot
  const maxInitRetries = 3;
  const [isInitializing, setIsInitializing] = useState(false);

  // Exponential backoff state for event sending
  const sendRetryCountRef = useRef<number>(0);
  const maxSendRetries = 5;
  const lastSendAttemptRef = useRef<number>(0);
  const isSendingRef = useRef<boolean>(false); // Prevent concurrent sends

  // Deep merge sampling defaults with proper handling of mouseInteraction
  // Use useMemo to avoid recreating this object on every render
  const samplingConfig = useMemo(() => {
    const defaultSampling = {
      scroll: 500,
      media: 800,
      mouseInteraction: {
        MouseUp: false,
        MouseDown: false,
        Click: true,
        ContextMenu: false,
        DblClick: true,
        Focus: true,
        Blur: true,
        TouchStart: false,
        TouchEnd: false,
      },
      mousemove: false,
      input: "last" as const,
    };

    // Merge sampling configs, handling mouseInteraction specially
    return {
      ...defaultSampling,
      ...sampling,
    };
  }, [sampling]);

  const shouldRecord = useCallback(() => {
    if (!enabled) return false;
    if (typeof window === "undefined") return false;

    // Check if current path should be excluded
    const currentPath = window.location.pathname;
    const isExcluded = excludedPaths.some(path => {
      // Support wildcards in path patterns
      const regex = new RegExp(`^${path.replace(/\*/g, '.*')}$`);
      return regex.test(currentPath);
    });
    if (isExcluded) return false;

    // Apply sampling rate
    if (sessionSampleRate < 1.0) {
      const random = Math.random();
      if (random > sessionSampleRate) return false;
    }

    return true;
  }, [enabled, excludedPaths, sessionSampleRate]);

  const initializeSession = useCallback(async (): Promise<boolean> => {
    // If already initialized or currently initializing, return current state
    if (sessionInitializedRef.current || isInitializing) return sessionInitializedRef.current;

    // If we've permanently failed, don't try again
    if (initFailedRef.current) {
      console.warn("[SessionRecorder] Initialization has permanently failed, not retrying");
      return false;
    }

    // Check if we've exceeded retry limit
    if (initRetryCountRef.current >= maxInitRetries) {
      console.error(`[SessionRecorder] Exceeded maximum initialization retries (${maxInitRetries})`);
      initFailedRef.current = true; // Mark as permanently failed
      return false;
    }

    setIsInitializing(true);
    initRetryCountRef.current++;

    console.log(`[SessionRecorder] Attempting to initialize session (attempt ${initRetryCountRef.current}/${maxInitRetries})`);

    const sessionId = generateSessionId();
    sessionIdRef.current = sessionId;

    try {
      const metadata = {
        sessionId: sessionId,
        ...getSessionMetadata(domain),
      };

      const response = await fetch(`${basePath}/${SESSION_RECORDER_ENDPOINT}/init`, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
        },
        body: JSON.stringify(metadata),
      });

      if (response.status === 201) {
        sessionInitializedRef.current = true;
        initRetryCountRef.current = 0; // Reset retry count on success
        initFailedRef.current = false; // Reset failed flag on success

        // Store session ID in localStorage for reference
        if (typeof localStorage !== "undefined") {
          localStorage.setItem("currentRecordingSessionId", sessionId);
        }

        console.log("[SessionRecorder] Session initialized successfully:", sessionId);
        return true;
      } else {
        console.error(`[SessionRecorder] Failed to initialize session: ${response.status} (attempt ${initRetryCountRef.current}/${maxInitRetries})`);
        sessionIdRef.current = ""; // Clear invalid session ID

        // Mark as permanently failed if we've hit the retry limit
        if (initRetryCountRef.current >= maxInitRetries) {
          initFailedRef.current = true;
        }

        return false;
      }
    } catch (error) {
      console.error(`[SessionRecorder] Failed to initialize session: ${error} (attempt ${initRetryCountRef.current}/${maxInitRetries})`);
      sessionIdRef.current = ""; // Clear invalid session ID

      // Mark as permanently failed if we've hit the retry limit
      if (initRetryCountRef.current >= maxInitRetries) {
        initFailedRef.current = true;
      }

      return false;
    } finally {
      setIsInitializing(false);
    }
  }, [basePath, domain, isInitializing]);

  const sendEvents = useCallback(async (isReliable = false): Promise<void> => {
    if (!sessionInitializedRef.current) {
      console.warn("[SessionRecorder] Cannot send events - session not initialized");
      return;
    }

    const events = eventsRef.current;
    if (events.length === 0) return;

    // Prevent concurrent sends
    if (isSendingRef.current && !isReliable) {
      console.log("[SessionRecorder] Already sending events, skipping this attempt");
      return;
    }

    // Check if we should apply backoff
    const now = Date.now();
    if (sendRetryCountRef.current > 0 && !isReliable) {
      const backoffMs = Math.min(1000 * Math.pow(2, sendRetryCountRef.current), 30000);
      const timeSinceLastAttempt = now - lastSendAttemptRef.current;

      if (timeSinceLastAttempt < backoffMs) {
        console.log(`[SessionRecorder] Backing off, waiting ${backoffMs - timeSinceLastAttempt}ms before retry`);
        return;
      }
    }

    isSendingRef.current = true;
    lastSendAttemptRef.current = now;

    const eventsToSend = [...events];
    // Don't clear the array immediately - clear only after successful send

    try {
      // Pack the events for more efficient transmission
      // Type assertion needed because rrweb's pack function expects eventWithTime but we have eventWithTime[]
      const packedEvents = pack(eventsToSend as unknown as Parameters<typeof pack>[0]);
      // Base64 encode the packed string for safe JSON transmission
      const encodedEvents = btoa(packedEvents);

      const payload = {
        sessionId: sessionIdRef.current,
        events: encodedEvents,
      };

      const url = `${basePath}/${SESSION_RECORDER_ENDPOINT}/events`;

      if (isReliable) {
        // Use sendBeacon for reliable sending on page unload
        if (navigator.sendBeacon) {
          const blob = new Blob([JSON.stringify(payload)], { type: "application/json" });
          const sent = navigator.sendBeacon(url, blob);
          if (!sent) {
            // Fallback to fetch with keepalive
            await fetch(url, {
              method: "POST",
              headers: { "Content-Type": "application/json" },
              body: JSON.stringify(payload),
              keepalive: true,
            });
          }
          // For reliable sends, we don't clear events as they're sent via beacon
          // which doesn't provide response feedback
          eventsRef.current = [];
          sendRetryCountRef.current = 0; // Reset retry count on success
        } else {
          // Fallback to fetch with keepalive
          await fetch(url, {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify(payload),
            keepalive: true,
          });
          // Clear events after successful send
          eventsRef.current = [];
          sendRetryCountRef.current = 0; // Reset retry count on success
        }
      } else {
        const response = await fetch(url, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(payload),
        });

        if (response.status === 404) {
          // Session not found, mark as not initialized but don't auto-reinitialize
          console.error("[SessionRecorder] Session not found on server, stopping recording");
          sessionInitializedRef.current = false;
          sessionIdRef.current = "";

          // Clear the session from localStorage
          if (typeof localStorage !== "undefined") {
            localStorage.removeItem("currentRecordingSessionId");
          }

          // Stop recording to prevent further errors
          if (stopFnRef.current) {
            stopFnRef.current();
            stopFnRef.current = null;
          }

          // Don't restore events - they're lost
          eventsRef.current = [];
          sendRetryCountRef.current = 0; // Reset retry count
        } else if (!response.ok) {
          console.error("[SessionRecorder] Failed to send events:", response.status);

          // Increment retry count with exponential backoff
          sendRetryCountRef.current++;

          if (sendRetryCountRef.current >= maxSendRetries) {
            console.error(`[SessionRecorder] Exceeded max send retries (${maxSendRetries}), dropping ${events.length} events`);
            eventsRef.current = []; // Drop events after too many failures
            sendRetryCountRef.current = 0; // Reset for next batch
          } else {
            console.log(`[SessionRecorder] Will retry (${sendRetryCountRef.current}/${maxSendRetries}) with exponential backoff`);
            // Keep events in the buffer for retry
          }
        } else {
          // Clear events after successful send
          eventsRef.current = [];
          sendRetryCountRef.current = 0; // Reset retry count on success
        }
      }
    } catch (error) {
      console.error("[SessionRecorder] Failed to send session events:", error);

      // Increment retry count with exponential backoff
      sendRetryCountRef.current++;

      if (sendRetryCountRef.current >= maxSendRetries) {
        console.error(`[SessionRecorder] Exceeded max send retries (${maxSendRetries}) after error, dropping ${events.length} events`);
        eventsRef.current = []; // Drop events after too many failures
        sendRetryCountRef.current = 0; // Reset for next batch
      } else {
        console.log(`[SessionRecorder] Will retry (${sendRetryCountRef.current}/${maxSendRetries}) with exponential backoff`);
        // Keep events in the buffer for retry
      }
    } finally {
      isSendingRef.current = false;
    }
  }, [basePath]);

  const scheduleFlush = useCallback(() => {
    // Clear any existing timeout
    if (flushTimeoutRef.current) {
      clearTimeout(flushTimeoutRef.current);
    }

    // Schedule the next flush after the interval
    flushTimeoutRef.current = setTimeout(() => {
      // Only send if we have events
      if (eventsRef.current.length > 0) {
        sendEvents(false);
      }
      // Reschedule for the next interval
      scheduleFlush();
    }, flushInterval);
  }, [sendEvents, flushInterval]);

  const startRecording = useCallback(async () => {
    if (stopFnRef.current) return; // Already recording
    if (!shouldRecord()) return;

    // If initialization has permanently failed, don't try to start
    if (initFailedRef.current) {
      console.warn("[SessionRecorder] Not starting recording - initialization has permanently failed");
      return;
    }

    // Initialize session first
    const initialized = await initializeSession();
    if (!initialized) {
      console.error("[SessionRecorder] Failed to initialize session, not starting recording");
      return;
    }

    // Don't clear events buffer on restart - preserve any pending events
    if (!eventsRef.current) {
      eventsRef.current = [];
    }

    const stopFn = record({
      emit(event: eventWithTime) {
        eventsRef.current.push(event);

        // Send when we have accumulated a full batch (whichever comes first: time or size)
        if (eventsRef.current.length >= batchSize) {
          sendEvents(false);
          // Reset the flush timer since we just sent
          scheduleFlush();
        }
      },
      sampling: samplingConfig,
      blockSelector,
      ignoreSelector,
      recordCanvas,
      collectFonts,
      maskAllInputs,
      maskInputOptions,
      maskTextSelector,
      blockClass,
      ignoreClass,
      maskTextClass,
      slimDOMOptions,
      checkoutEveryNms: 30000,
      checkoutEveryNth: 200,
    });

    if (stopFn) {
      stopFnRef.current = stopFn;
      // Store the takeFullSnapshot function from rrweb
      // The record function returns an object with both stop and takeFullSnapshot methods
      takeSnapshotRef.current = (stopFn as unknown as Record<string, unknown>).takeFullSnapshot as (() => void) || null;
    }

    // Start the flush interval
    scheduleFlush();
  }, [shouldRecord, initializeSession, sendEvents, scheduleFlush, batchSize, blockSelector, ignoreSelector, recordCanvas, collectFonts, maskAllInputs, maskInputOptions, maskTextSelector, blockClass, ignoreClass, maskTextClass, slimDOMOptions, samplingConfig]);

  const stopRecording = useCallback(() => {
    if (stopFnRef.current) {
      stopFnRef.current();
      stopFnRef.current = null;

      // Clear flush timeout
      if (flushTimeoutRef.current) {
        clearTimeout(flushTimeoutRef.current);
        flushTimeoutRef.current = null;
      }

      // Send any remaining events reliably
      sendEvents(true);

      // Clear session state
      sessionInitializedRef.current = false;
      sessionIdRef.current = "";

      // Clear session ID from localStorage
      if (typeof localStorage !== "undefined") {
        localStorage.removeItem("currentRecordingSessionId");
      }
    }

    // Reset retry counters when explicitly stopping
    initRetryCountRef.current = 0;
    initFailedRef.current = false;
  }, [sendEvents]);

  // Effect to handle recording lifecycle
  useEffect(() => {
    // Early return if we shouldn't record or have permanently failed
    if (!enabled || initFailedRef.current || stopFnRef.current || sessionInitializedRef.current) {
      return;
    }

    // Check if current path should be excluded
    const currentPath = window.location.pathname;
    const isExcluded = excludedPaths.some(path => {
      const regex = new RegExp(`^${path.replace(/\*/g, '.*')}$`);
      return regex.test(currentPath);
    });

    if (isExcluded) {
      return;
    }

    // Apply sampling rate
    if (sessionSampleRate < 1.0) {
      const random = Math.random();
      if (random > sessionSampleRate) {
        return;
      }
    }

    // Now we can start recording
    startRecording();

    // Handle page unload
    const handleUnload = (): void => {
      if (sessionInitializedRef.current && eventsRef.current.length > 0) {
        sendEvents(true);
      }
    };

    window.addEventListener("beforeunload", handleUnload);
    window.addEventListener("pagehide", handleUnload);

    return () => {
      window.removeEventListener("beforeunload", handleUnload);
      window.removeEventListener("pagehide", handleUnload);
    };
    // Dependencies that should trigger re-evaluation
  }, [enabled, excludedPaths, sessionSampleRate, startRecording, sendEvents]);

  // Monitor location changes to stop/start recording based on excluded paths
  useEffect(() => {
    const checkPathAndToggleRecording = (): void => {
      if (!enabled || initFailedRef.current) {
        if (!enabled) {
          stopRecording();
        }
        return;
      }

      const currentPath = window.location.pathname;
      const isExcluded = excludedPaths.some(path => {
        const regex = new RegExp(`^${path.replace(/\*/g, '.*')}$`);
        return regex.test(currentPath);
      });

      // Only stop/start recording if we're actually changing exclusion status
      // This prevents unnecessary restarts during normal navigation
      const isCurrentlyRecording = stopFnRef.current !== null;

      if (isExcluded && isCurrentlyRecording) {
        // Path is excluded and we're recording - stop
        stopRecording();
      } else if (!isExcluded && !isCurrentlyRecording && !initFailedRef.current) {
        // Path is not excluded and we're not recording - start
        startRecording();
      }
      // If the path exclusion status hasn't changed, keep recording continuously
    };

    // Listen for route changes
    const originalPushState = window.history.pushState;
    const originalReplaceState = window.history.replaceState;

    window.history.pushState = function(...args) {
      // Flush current events before route change to ensure they're captured
      if (stopFnRef.current && eventsRef.current.length > 0) {
        sendEvents(false);
      }
      originalPushState.apply(window.history, args);
      // Use setTimeout to allow DOM updates to complete, then take snapshot
      setTimeout(() => {
        checkPathAndToggleRecording();
        // Force a full snapshot after route change to capture new page state
        // This ensures the replay shows the correct content after navigation
        if (takeSnapshotRef.current) {
          takeSnapshotRef.current();
        }
      }, 100); // Small delay to ensure DOM is updated
    };

    window.history.replaceState = function(...args) {
      // Flush current events before route change to ensure they're captured
      if (stopFnRef.current && eventsRef.current.length > 0) {
        sendEvents(false);
      }
      originalReplaceState.apply(window.history, args);
      // Use setTimeout to allow DOM updates to complete, then take snapshot
      setTimeout(() => {
        checkPathAndToggleRecording();
        // Force a full snapshot after route change to capture new page state
        // This ensures the replay shows the correct content after navigation
        if (takeSnapshotRef.current) {
          takeSnapshotRef.current();
        }
      }, 100); // Small delay to ensure DOM is updated
    };

    const handlePopState = (): void => {
      // Flush current events before route change to ensure they're captured
      if (stopFnRef.current && eventsRef.current.length > 0) {
        sendEvents(false);
      }
      // Use setTimeout to allow DOM updates to complete, then take snapshot
      setTimeout(() => {
        checkPathAndToggleRecording();
        // Force a full snapshot after route change to capture new page state
        // This ensures the replay shows the correct content after navigation
        if (takeSnapshotRef.current) {
          takeSnapshotRef.current();
        }
      }, 100); // Small delay to ensure DOM is updated
    };

    window.addEventListener("popstate", handlePopState);

    return () => {
      window.history.pushState = originalPushState;
      window.history.replaceState = originalReplaceState;
      window.removeEventListener("popstate", handlePopState);
    };
  }, [enabled, excludedPaths, startRecording, stopRecording, sendEvents]);

  return null;
}
