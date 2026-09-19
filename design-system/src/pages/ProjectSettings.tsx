// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from "react";
import { useForm, Controller } from "react-hook-form";
import { zodResolver } from "@hookform/resolvers/zod";
import { z } from "zod";
import {
  Button,
  Disclosure,
  Field,
  Settings,
  SettingsGroup as Group,
} from "@temps-sdk/ds";
import {
  Input,
  Switch,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@temps-sdk/ui";

const schema = z.object({
  url: z.url({
    protocol: /^https?$/,
    error: "Enter a valid HTTP or HTTPS URL.",
  }),
  domain: z.string().trim().min(1, "Enter a preview domain."),
  email: z.union([z.literal(""), z.email("Enter a valid email address.")]),
  certificates: z.enum(["production", "staging"]),
  screenshots: z.boolean(),
});

const INITIAL: z.infer<typeof schema> = {
  url: "https://console.example.com",
  domain: "apps.example.com",
  email: "ops@example.com",
  certificates: "production",
  screenshots: true,
};

export default function ProjectSettings() {
  const [saved, setSaved] = useState(false);
  const [refreshed, setRefreshed] = useState(false);
  const {
    register,
    control,
    watch,
    reset,
    handleSubmit,
    formState: { errors: fieldErrors, isDirty: dirty },
  } = useForm<z.infer<typeof schema>>({
    resolver: zodResolver(schema),
    defaultValues: INITIAL,
    mode: "onChange",
  });
  const values = watch();
  const errors = {
    "External URL": fieldErrors.url?.message,
    "Preview domain": fieldErrors.domain?.message,
    "Contact email": fieldErrors.email?.message,
  };
  return (
    <Settings
      title="Platform settings"
      description="Interactive sample · changes stay in this example"
      dirty={dirty}
      errors={errors}
      onSubmit={handleSubmit((values) => {
        reset(values);
        setSaved(true);
      })}
    >
      <div className="max-w-5xl space-y-10">
        <Group title="Platform">
          <Field
            label="External URL"
            error={fieldErrors.url?.message}
            help={{
              label: "About the external URL",
              content:
                "Used for OAuth callbacks, webhooks, and external integrations.",
            }}
          >
            {(props) => <Input {...props} type="url" {...register("url")} />}
          </Field>
          <Field
            label="Preview domain"
            error={fieldErrors.domain?.message}
            description="New deployments receive a subdomain here."
          >
            {(props) => <Input {...props} {...register("domain")} />}
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
              <Input {...props} type="email" {...register("email")} />
            )}
          </Field>
          <Field label="Certificate environment">
            {(props) => (
              <Controller
                name="certificates"
                control={control}
                render={({ field }) => (
                  <Select value={field.value} onValueChange={field.onChange}>
                    <SelectTrigger {...props}>
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="production">Production</SelectItem>
                      <SelectItem value="staging">Staging</SelectItem>
                    </SelectContent>
                  </Select>
                )}
              />
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
              <Controller
                name="screenshots"
                control={control}
                render={({ field }) => (
                  <Switch
                    {...props}
                    checked={field.value}
                    onCheckedChange={field.onChange}
                    onBlur={field.onBlur}
                    ref={field.ref}
                  />
                )}
              />
            )}
          </Field>
          <div hidden={!values.screenshots}>
            <Disclosure label="Capture details">
              <p>
                A screenshot is captured after each successful deployment. This
                sample does not run a capture.
              </p>
            </Disclosure>
          </div>
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
