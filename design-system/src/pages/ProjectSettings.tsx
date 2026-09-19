// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useRef, useState } from "react";
import { Disclosure, Field, Settings, SettingsSection } from "@temps-sdk/ds";
import { Settings2, Bell } from "lucide-react";
import { Input } from "@temps-sdk/ui";

const INITIAL = { name: "checkout-api", notifyEmail: "" };

/** Reference screen for the `Settings` template: `Field`s + sticky save bar. */
export default function ProjectSettings() {
  const [values, setValues] = useState(INITIAL);
  const [baseline, setBaseline] = useState(INITIAL);
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  useEffect(() => () => clearTimeout(timer.current), []);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);

  const dirty =
    values.name !== baseline.name ||
    values.notifyEmail !== baseline.notifyEmail;

  const errors: Record<string, string | undefined> = {
    name:
      values.name.trim().length === 0 ? "Project name is required." : undefined,
    notifyEmail:
      values.notifyEmail && !values.notifyEmail.includes("@")
        ? "Enter a valid email address."
        : undefined,
  };

  const hasErrors = Object.values(errors).some(Boolean);

  return (
    <Settings
      title="Project settings"
      description="Sample project"
      errors={errors}
      dirty={dirty && !hasErrors}
      saving={saving}
      onSubmit={(e) => {
        e.preventDefault();
        if (hasErrors || !dirty || saving) return;
        setSaved(false);
        setSaving(true);
        timer.current = setTimeout(() => {
          setBaseline(values);
          setSaving(false);
          setSaved(true);
        }, 900);
      }}
    >
      <Disclosure label="About this example">
        <p>
          Try editing, validation, and saving. Changes stay in this example.
        </p>
      </Disclosure>
      <SettingsSection
        title="General"
        icon={Settings2}
        defaultOpen
        hasError={!!errors.name}
      >
        <Field label="Project name" error={errors.name}>
          {(fieldProps) => (
            <Input
              {...fieldProps}
              readOnly={saving}
              value={values.name}
              onChange={(e) =>
                setValues((v) => ({ ...v, name: e.target.value }))
              }
            />
          )}
        </Field>
      </SettingsSection>
      <SettingsSection
        title="Notifications"
        icon={Bell}
        hasError={!!errors.notifyEmail}
      >
        <Field
          label="Deploy notification email"
          optional
          help={{
            label: "About deployment notifications",
            content: "Sent when a deployment to this project fails.",
          }}
          error={errors.notifyEmail}
        >
          {(fieldProps) => (
            <Input
              {...fieldProps}
              readOnly={saving}
              type="email"
              placeholder="you@example.com"
              value={values.notifyEmail}
              onChange={(e) =>
                setValues((v) => ({ ...v, notifyEmail: e.target.value }))
              }
            />
          )}
        </Field>
      </SettingsSection>
      <p role="status" className="text-sm text-muted-foreground">
        {saving
          ? "Saving sample changes…"
          : dirty
            ? "You have unsaved changes."
            : saved
              ? "Sample changes saved."
              : "No unsaved changes."}
      </p>
    </Settings>
  );
}
