// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The default survey widget: a small card in the bottom-right corner,
 * rendered in a shadow root so the site's CSS cannot reach it and its CSS
 * cannot reach the site. No dependencies; every control is a native form
 * element, so it works with a keyboard and a screen reader.
 */

import {
  MAX_OPEN_TEXT_LENGTH,
  surveyScaleBounds,
  type Survey,
  type SurveyAnswers,
  type SurveyQuestion,
} from "./Surveys";

export interface SurveyWidgetHandlers {
  /** Return the per-question errors to show, or none when sent. */
  onSubmit: (answers: SurveyAnswers) => Record<string, string> | null;
  onDismiss: () => void;
}

const STYLE = `
:host { all: initial; }
.card { position: fixed; right: 16px; bottom: 16px; z-index: 2147483000; width: min(360px, calc(100vw - 32px));
  max-height: calc(100vh - 32px); overflow: auto; box-sizing: border-box; padding: 16px; border-radius: 10px;
  background: #fff; color: #111; border: 1px solid #ddd; box-shadow: 0 8px 30px rgba(0,0,0,.15);
  font: 14px/1.4 system-ui, -apple-system, "Segoe UI", sans-serif; }
@media (prefers-color-scheme: dark) { .card { background: #1c1c1e; color: #f2f2f2; border-color: #3a3a3c; } }
.head { display: flex; justify-content: space-between; align-items: start; gap: 8px; margin-bottom: 8px; }
h2 { font-size: 15px; margin: 0; }
fieldset { border: 0; padding: 0; margin: 0 0 12px; }
legend { font-weight: 600; margin-bottom: 6px; padding: 0; }
.scale { display: flex; flex-wrap: wrap; gap: 4px; }
.scale label { flex: 1 0 28px; text-align: center; border: 1px solid #ccc; border-radius: 6px; padding: 4px 0; cursor: pointer; }
.scale input { position: absolute; opacity: 0; pointer-events: none; }
.scale label:has(input:checked) { background: #111; color: #fff; border-color: #111; }
.scale label:has(input:focus-visible) { outline: 2px solid #2563eb; outline-offset: 1px; }
.labels { display: flex; justify-content: space-between; font-size: 12px; opacity: .7; margin-top: 2px; }
.choice { display: flex; gap: 6px; align-items: center; margin: 2px 0; }
textarea { width: 100%; box-sizing: border-box; min-height: 64px; font: inherit; padding: 6px; border-radius: 6px; border: 1px solid #ccc; }
.error { color: #b91c1c; font-size: 12px; margin-top: 4px; }
button { font: inherit; cursor: pointer; border-radius: 6px; }
.close { background: none; border: 0; font-size: 18px; line-height: 1; color: inherit; padding: 0 4px; }
.send { width: 100%; padding: 8px; background: #111; color: #fff; border: 0; }
.thanks { margin: 0; }
`;

/** Render `survey`; returns a function that removes it. */
export function renderSurveyWidget(survey: Survey, handlers: SurveyWidgetHandlers): () => void {
  if (typeof document === "undefined") return () => undefined;
  const host = document.createElement("div");
  host.setAttribute("data-temps-survey", String(survey.id));
  const root = host.attachShadow({ mode: "open" });
  const style = document.createElement("style");
  style.textContent = STYLE;
  root.appendChild(style);

  const titleId = `temps-survey-${survey.id}-title`;
  const card = element("form", { class: "card", role: "dialog", "aria-labelledby": titleId });
  card.noValidate = true;
  const head = element("div", { class: "head" });
  const title = element("h2", { id: titleId });
  title.textContent = survey.name;
  const close = element("button", { type: "button", class: "close", "aria-label": "Close survey" });
  close.textContent = "×";
  head.append(title, close);
  card.appendChild(head);

  const errorSlots = new Map<string, HTMLElement>();
  for (const question of survey.questions) {
    const { fieldset, error } = renderQuestion(survey.id, question);
    errorSlots.set(question.id, error);
    card.appendChild(fieldset);
  }
  const send = element("button", { type: "submit", class: "send" });
  send.textContent = "Send";
  card.appendChild(send);
  root.appendChild(card);
  document.body.appendChild(host);

  let removed = false;
  const remove = () => {
    if (removed) return;
    removed = true;
    document.removeEventListener("keydown", onKey);
    host.remove();
  };
  const dismiss = () => {
    handlers.onDismiss();
    remove();
  };
  const onKey = (event: KeyboardEvent) => {
    if (event.key === "Escape") dismiss();
  };
  document.addEventListener("keydown", onKey);
  close.addEventListener("click", dismiss);

  card.addEventListener("submit", (event) => {
    event.preventDefault();
    const errors = handlers.onSubmit(collectAnswers(card, survey));
    for (const [id, slot] of errorSlots) slot.textContent = errors?.[id] ?? "";
    if (errors) return;
    card.replaceChildren(head);
    close.addEventListener("click", remove);
    const thanks = element("p", { class: "thanks", role: "status" });
    thanks.textContent = "Thanks for your answer.";
    card.appendChild(thanks);
    setTimeout(remove, 2500);
  });

  return remove;
}

function renderQuestion(surveyId: number, question: SurveyQuestion): { fieldset: HTMLElement; error: HTMLElement } {
  const name = `q-${surveyId}-${question.id}`;
  const fieldset = element("fieldset", {});
  const legend = element("legend", {});
  legend.textContent = question.optional ? `${question.prompt} (optional)` : question.prompt;
  fieldset.appendChild(legend);

  switch (question.kind) {
    case "open_text": {
      const textarea = element("textarea", { name, maxlength: String(MAX_OPEN_TEXT_LENGTH), "aria-label": question.prompt });
      fieldset.appendChild(textarea);
      break;
    }
    case "rating":
    case "nps": {
      const [min, max] = surveyScaleBounds(question);
      const scale = element("div", { class: "scale" });
      for (let value = min; value <= max; value += 1) {
        const label = element("label", {});
        const input = element("input", { type: "radio", name, value: String(value) });
        label.append(input, document.createTextNode(String(value)));
        scale.appendChild(label);
      }
      fieldset.appendChild(scale);
      if (question.lower_label || question.upper_label) {
        const labels = element("div", { class: "labels" });
        const low = element("span", {});
        low.textContent = question.lower_label ?? "";
        const high = element("span", {});
        high.textContent = question.upper_label ?? "";
        labels.append(low, high);
        fieldset.appendChild(labels);
      }
      break;
    }
    case "single_choice":
    case "multiple_choice": {
      const type = question.kind === "single_choice" ? "radio" : "checkbox";
      for (const choice of question.choices ?? []) {
        const label = element("label", { class: "choice" });
        const input = element("input", { type, name, value: choice });
        label.append(input, document.createTextNode(choice));
        fieldset.appendChild(label);
      }
      break;
    }
  }
  const error = element("div", { class: "error", role: "alert" });
  fieldset.appendChild(error);
  return { fieldset, error };
}

function collectAnswers(form: HTMLFormElement, survey: Survey): SurveyAnswers {
  const answers: SurveyAnswers = {};
  for (const question of survey.questions) {
    const name = `q-${survey.id}-${question.id}`;
    const fields = Array.from(form.querySelectorAll<HTMLInputElement | HTMLTextAreaElement>(`[name="${cssEscape(name)}"]`));
    switch (question.kind) {
      case "open_text":
        answers[question.id] = fields[0]?.value ?? "";
        break;
      case "rating":
      case "nps": {
        const picked = fields.find((field) => (field as HTMLInputElement).checked);
        if (picked) answers[question.id] = Number(picked.value);
        break;
      }
      case "single_choice": {
        const picked = fields.find((field) => (field as HTMLInputElement).checked);
        if (picked) answers[question.id] = picked.value;
        break;
      }
      case "multiple_choice":
        answers[question.id] = fields
          .filter((field) => (field as HTMLInputElement).checked)
          .map((field) => field.value);
        break;
    }
  }
  return answers;
}

/** Question ids are `[a-z0-9_]`, so names need no escaping beyond quotes. */
function cssEscape(value: string): string {
  return value.replace(/["\\]/g, "\\$&");
}

function element<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  attributes: Record<string, string>
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  for (const [name, value] of Object.entries(attributes)) node.setAttribute(name, value);
  return node;
}
