// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * In-app surveys (ADR 110 D1).
 *
 * Fetches the surveys this visitor may see from `{basePath}/surveys` on the
 * page's own origin. The server has already applied sampling and flag
 * conditions; this class applies the conditions that depend on the browser
 * (page path, a triggering event, once per visitor) and records answers as
 * ordinary analytics events:
 *
 * - `survey_shown`     `{ survey_id, survey_name }`
 * - `survey_dismissed` `{ survey_id, survey_name }`
 * - `survey_sent`      `{ survey_id, survey_name, responses: { [questionId]: answer } }`
 *
 * Rendering is separate (`SurveyWidget.ts`), so an app can draw its own UI
 * with `eligible()`, `markShown()`, `dismiss()` and `submit()`.
 */

import type { JsonValue } from "./types";

export type SurveyQuestionKind =
  | "open_text"
  | "rating"
  | "nps"
  | "single_choice"
  | "multiple_choice";

export interface SurveyQuestion {
  id: string;
  kind: SurveyQuestionKind;
  prompt: string;
  optional?: boolean;
  choices?: string[];
  /** Top of a `rating` scale (3, 5, 7 or 10). Defaults to 5. */
  scale?: number;
  lower_label?: string;
  upper_label?: string;
}

export interface SurveyDisplay {
  path_pattern?: string;
  trigger_event?: string;
  once_per_visitor: boolean;
}

export interface Survey {
  id: number;
  name: string;
  questions: SurveyQuestion[];
  display: SurveyDisplay;
}

/** Open text or a single choice: a string. Rating and NPS: a number.
 * Multiple choice: the selected choices. */
export type SurveyAnswer = string | number | string[];
export type SurveyAnswers = Record<string, SurveyAnswer>;

export type SurveysStatus = "idle" | "loading" | "ready" | "error";

/** Longest open-text answer sent; the server cuts anything longer. */
export const MAX_OPEN_TEXT_LENGTH = 2000;
const SEEN_STORAGE_KEY = "temps_surveys_seen";
/** Surveys remembered as seen; the oldest are forgotten first. */
const MAX_SEEN = 200;

export interface SurveysOptions {
  /** Same-origin API prefix. Defaults to `/api/_temps`. */
  basePath?: string;
  /** Subject for sampling and flag conditions; defaults to the visitor cookie. */
  key?: string;
  /** Attributes for flag conditions. */
  attributes?: Record<string, string | number | boolean>;
  /** Records a survey event. `createAnalytics` passes `trackEvent`. */
  onEvent: (name: string, data: Record<string, JsonValue>) => void;
  onError?: (error: Error) => void;
  /** Never fetch or show anything. */
  disabled?: boolean;
  /** Test seams. */
  fetch?: typeof fetch;
  storage?: Pick<Storage, "getItem" | "setItem"> | null;
}

interface SurveysResponseBody {
  surveys?: Survey[];
}

export class Surveys {
  private readonly basePath: string;
  private readonly onEvent: SurveysOptions["onEvent"];
  private readonly onError: (error: Error) => void;
  private readonly fetchImpl?: typeof fetch;
  private readonly storage: Pick<Storage, "getItem" | "setItem"> | null;
  private readonly disabled: boolean;
  private readonly context: { key?: string; attributes?: SurveysOptions["attributes"] };

  private surveys: Survey[] = [];
  private inflight: Promise<void> | null = null;
  private currentStatus: SurveysStatus = "idle";
  private listeners = new Set<() => void>();
  private revision = 0;
  /** Shown during this page's lifetime, whatever `once_per_visitor` says,
   * so one page never shows a survey twice. */
  private shownHere = new Set<number>();

  constructor(options: SurveysOptions) {
    this.basePath = options.basePath ?? "/api/_temps";
    this.onEvent = options.onEvent;
    this.onError =
      options.onError ??
      ((error) => {
        // eslint-disable-next-line no-console
        console.warn("[temps-surveys] no surveys shown:", error.message);
      });
    this.fetchImpl = options.fetch;
    this.storage = options.storage === undefined ? defaultStorage() : options.storage;
    this.disabled = options.disabled ?? false;
    this.context = { key: options.key, attributes: options.attributes };
  }

  get status(): SurveysStatus {
    return this.currentStatus;
  }

  /** Changes whenever the fetched surveys change. */
  get version(): number {
    return this.revision;
  }

  /** Every survey this visitor may see on some page. */
  all(): Survey[] {
    return [...this.surveys];
  }

  /** Fetch once; later calls return the same promise. */
  load(): Promise<void> {
    if (this.inflight) return this.inflight;
    if (this.disabled) return Promise.resolve();
    const fetchImpl = this.fetchImpl ?? (typeof fetch === "function" ? fetch : undefined);
    if (!fetchImpl) {
      this.currentStatus = "error";
      return Promise.resolve();
    }
    this.currentStatus = "loading";
    this.inflight = (async () => {
      try {
        const response = await fetchImpl(`${this.basePath}/surveys`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          credentials: "same-origin",
          body: JSON.stringify({ key: this.context.key, attributes: this.context.attributes ?? {} }),
        });
        if (!response.ok) throw new Error(`surveys request returned ${response.status}`);
        const parsed = (await response.json()) as SurveysResponseBody;
        this.surveys = Array.isArray(parsed.surveys) ? parsed.surveys : [];
        this.currentStatus = "ready";
      } catch (error) {
        this.currentStatus = "error";
        this.onError(error instanceof Error ? error : new Error(String(error)));
      }
      this.notify();
    })();
    return this.inflight;
  }

  /**
   * Surveys to show now on `pathname`. With `event`, only surveys triggered
   * by that event; without it, only surveys shown on page load.
   */
  eligible(pathname: string, event?: string): Survey[] {
    const seen = this.seen();
    return this.surveys.filter((survey) => {
      const trigger = survey.display.trigger_event;
      if (event === undefined ? Boolean(trigger) : trigger !== event) return false;
      if (survey.display.path_pattern && !pathMatches(survey.display.path_pattern, pathname)) {
        return false;
      }
      if (this.shownHere.has(survey.id)) return false;
      return !(survey.display.once_per_visitor && seen.has(survey.id));
    });
  }

  /** Record that the survey was displayed. */
  markShown(survey: Survey): void {
    this.shownHere.add(survey.id);
    if (survey.display.once_per_visitor) this.remember(survey.id);
    this.emit("survey_shown", survey);
  }

  /** Record that the visitor closed the survey without answering. */
  dismiss(survey: Survey): void {
    this.emit("survey_dismissed", survey);
  }

  /**
   * Validate and record the visitor's answers. Returns the problems instead
   * of sending when an answer is missing or malformed.
   */
  submit(survey: Survey, answers: SurveyAnswers): { ok: true } | { ok: false; errors: Record<string, string> } {
    const checked = validateAnswers(survey, answers);
    if (!checked.ok) return checked;
    this.emit("survey_sent", survey, { responses: checked.responses });
    return { ok: true };
  }

  subscribe(listener: () => void): () => void {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  }

  private emit(name: string, survey: Survey, extra: Record<string, JsonValue> = {}): void {
    try {
      this.onEvent(name, { survey_id: survey.id, survey_name: survey.name, ...extra });
    } catch {
      // Recording is best effort; the visitor's UI must not break.
    }
  }

  private seen(): Set<number> {
    try {
      const raw = this.storage?.getItem(SEEN_STORAGE_KEY);
      const parsed: unknown = raw ? JSON.parse(raw) : [];
      return new Set(Array.isArray(parsed) ? parsed.filter((id): id is number => typeof id === "number") : []);
    } catch {
      return new Set();
    }
  }

  private remember(id: number): void {
    try {
      const ids = [...this.seen()].filter((seen) => seen !== id);
      ids.push(id);
      this.storage?.setItem(SEEN_STORAGE_KEY, JSON.stringify(ids.slice(-MAX_SEEN)));
    } catch {
      // Storage may be full or blocked; the survey is then shown again later.
    }
  }

  private notify(): void {
    this.revision += 1;
    for (const listener of [...this.listeners]) {
      try {
        listener();
      } catch {
        // A failing subscriber must not stop the others.
      }
    }
  }
}

function defaultStorage(): Pick<Storage, "getItem" | "setItem"> | null {
  try {
    return typeof window !== "undefined" ? window.localStorage : null;
  } catch {
    return null;
  }
}

/** Whether `pathname` matches `pattern`, where `*` matches any run of
 * characters and the whole path must match. */
export function pathMatches(pattern: string, pathname: string): boolean {
  const parts = pattern.split("*");
  if (parts.length === 1) return pattern === pathname;
  const first = parts[0] ?? "";
  const last = parts[parts.length - 1] ?? "";
  if (!pathname.startsWith(first)) return false;
  let position = first.length;
  for (const middle of parts.slice(1, -1)) {
    const found = pathname.indexOf(middle, position);
    if (found === -1) return false;
    position = found + middle.length;
  }
  return pathname.length - position >= last.length && pathname.endsWith(last);
}

function scaleBounds(question: SurveyQuestion): [number, number] {
  return question.kind === "nps" ? [0, 10] : [1, question.scale ?? 5];
}

/** Check answers against the survey; keep only answers to its questions. */
export function validateAnswers(
  survey: Survey,
  answers: SurveyAnswers
): { ok: true; responses: Record<string, JsonValue> } | { ok: false; errors: Record<string, string> } {
  const errors: Record<string, string> = {};
  const responses: Record<string, JsonValue> = {};
  for (const question of survey.questions) {
    const answer = answers[question.id];
    const empty =
      answer === undefined ||
      answer === null ||
      (typeof answer === "string" && answer.trim() === "") ||
      (Array.isArray(answer) && answer.length === 0);
    if (empty) {
      if (!question.optional) errors[question.id] = "This question needs an answer.";
      continue;
    }
    switch (question.kind) {
      case "open_text":
        if (typeof answer !== "string") errors[question.id] = "Expected text.";
        else responses[question.id] = answer.slice(0, MAX_OPEN_TEXT_LENGTH);
        break;
      case "single_choice":
        if (typeof answer !== "string" || !(question.choices ?? []).includes(answer)) {
          errors[question.id] = "Pick one of the choices.";
        } else responses[question.id] = answer;
        break;
      case "multiple_choice": {
        const choices = question.choices ?? [];
        if (!Array.isArray(answer) || answer.some((item) => !choices.includes(item))) {
          errors[question.id] = "Pick from the choices.";
        } else responses[question.id] = [...new Set(answer)];
        break;
      }
      case "rating":
      case "nps": {
        const [min, max] = scaleBounds(question);
        if (typeof answer !== "number" || !Number.isInteger(answer) || answer < min || answer > max) {
          errors[question.id] = `Pick a number from ${min} to ${max}.`;
        } else responses[question.id] = answer;
        break;
      }
    }
  }
  return Object.keys(errors).length > 0 ? { ok: false, errors } : { ok: true, responses };
}

export { scaleBounds as surveyScaleBounds };
