// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState, type ReactNode } from "react";
import { Button, Disclosure, Field, Settings } from "@temps-sdk/ds";
import {
  Input,
  Switch,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@temps-sdk/ui";

const INITIAL = {
  url: "https://console.example.com",
  domain: "apps.example.com",
  email: "ops@example.com",
  certificates: "production",
  screenshots: true,
};

function Group({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="grid min-w-0 gap-5 md:grid-cols-[minmax(0,1fr)_minmax(0,2fr)] md:gap-10">
      <h2 className="text-base font-semibold">{title}</h2>
      <div className="min-w-0 space-y-5">{children}</div>
    </section>
  );
}

export default function ProjectSettings() {
  const [values, setValues] = useState(INITIAL);
  const [baseline, setBaseline] = useState(INITIAL);
  const [saved, setSaved] = useState(false);
  const [refreshed, setRefreshed] = useState(false);
  const dirty = JSON.stringify(values) !== JSON.stringify(baseline);
  let urlError: string | undefined;
  try {
    if (!["http:", "https:"].includes(new URL(values.url).protocol))
      urlError = "Use an HTTP or HTTPS URL.";
  } catch {
    urlError = "Enter a valid URL.";
  }
  const errors = {
    "External URL": urlError,
    "Contact email":
      values.email && !/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(values.email)
        ? "Enter a valid email address."
        : undefined,
  };
  return (
    <Settings
      title="Platform settings"
      description="Interactive sample · changes stay in this example"
      dirty={dirty}
      errors={errors}
      onSubmit={(event) => {
        event.preventDefault();
        if (!dirty || Object.values(errors).some(Boolean)) return;
        setBaseline(values);
        setSaved(true);
      }}
    >
      <div className="max-w-5xl space-y-10">
        <Group title="Platform">
          <Field
            label="External URL"
            error={urlError}
            help={{
              label: "About the external URL",
              content:
                "Used for OAuth callbacks, webhooks, and external integrations.",
            }}
          >
            {(props) => (
              <Input
                {...props}
                type="url"
                value={values.url}
                onChange={(e) =>
                  setValues((v) => ({ ...v, url: e.target.value }))
                }
              />
            )}
          </Field>
          <Field
            label="Preview domain"
            description="New deployments receive a subdomain here."
          >
            {(props) => (
              <Input
                {...props}
                value={values.domain}
                onChange={(e) =>
                  setValues((v) => ({ ...v, domain: e.target.value }))
                }
              />
            )}
          </Field>
        </Group>
        <Group title="Certificates">
          <Field
            label="Contact email"
            optional
            error={errors["Contact email"]}
            help={{
              label: "About certificate email",
              content:
                "The certificate authority uses this address for account notices.",
            }}
          >
            {(props) => (
              <Input
                {...props}
                type="email"
                value={values.email}
                onChange={(e) =>
                  setValues((v) => ({ ...v, email: e.target.value }))
                }
              />
            )}
          </Field>
          <Field label="Certificate environment">
            {(props) => (
              <Select
                value={values.certificates}
                onValueChange={(certificates) =>
                  setValues((v) => ({ ...v, certificates }))
                }
              >
                <SelectTrigger {...props}>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="production">Production</SelectItem>
                  <SelectItem value="staging">Staging</SelectItem>
                </SelectContent>
              </Select>
            )}
          </Field>
          {values.certificates === "staging" && (
            <p className="text-sm text-muted-foreground">
              Staging certificates are not trusted by browsers. Use them only
              for testing.
            </p>
          )}
        </Group>
        <Group title="Screenshots">
          <Field label="Capture deployment screenshots">
            {(props) => (
              <Switch
                {...props}
                checked={values.screenshots}
                onCheckedChange={(screenshots) =>
                  setValues((v) => ({ ...v, screenshots }))
                }
              />
            )}
          </Field>
          {values.screenshots && (
            <Disclosure label="Capture details">
              <p>
                A screenshot is captured after each successful deployment. This
                sample does not run a capture.
              </p>
            </Disclosure>
          )}
        </Group>
        <Group title="Route table">
          <p>
            The proxy uses the route table to send requests to deployments and
            services. Reload it from saved configuration if traffic is reaching
            an outdated destination.
          </p>
          <Button
            type="button"
            variant="outline"
            onClick={() => setRefreshed(true)}
          >
            {refreshed
              ? "Sample route table reloaded"
              : "Reload sample route table"}
          </Button>
        </Group>
        <p role="status" className="text-sm text-muted-foreground">
          {dirty
            ? "Unsaved changes"
            : saved
              ? "Sample changes saved"
              : "All changes saved"}
        </p>
      </div>
    </Settings>
  );
}
