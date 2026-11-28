// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { existsSync } from "node:fs";
import { basename, resolve } from "node:path";
import type { Command } from "commander";
import {
  createProject,
  deployFromImage,
  getEnvironments,
  getProject,
  getProjectBySlug,
} from "../../api/sdk.gen.js";
import { requireAuth } from "../../config/store.js";
import { client, setupClient, getErrorMessage } from "../../lib/api-client.js";
import { watchDeployment } from "../../lib/deployment-watcher.jsx";
import { normalizeEnvironmentsResponse } from "../deploy/deploy-image.js";
import {
  checkImageExists,
  dockerBuild,
  uploadLocalImage,
} from "../deploy/deploy-local-image.js";
import { PluginCommandError } from "./grants.js";
import type { PluginServicesResponse, ServicePlugin } from "./services.js";
import { updateLines } from "./updates.js";

/**
 * Replaced in tests: the real ones draw the deployment's progress and run
 * Docker on this machine.
 */
export const imageDeployment = {
  watch: watchDeployment,
  /** Whether the local Docker has this image. */
  exists: checkImageExists,
  build: dockerBuild,
  /** Export an image from the local Docker, upload it and start its deployment. */
  upload: uploadLocalImage,
};

/** How long a deployed update is looked for before the command says where to find it. */
const UPDATE_LOOKUPS = 10;

interface AddedServicePlugin {
  name: string;
  version: string;
  display_name?: string | null;
  project_id: number;
  environment_id: number;
}

interface PluginImageOptions {
  image?: string;
  localImage?: string;
  build?: boolean;
  dockerfile?: string;
  context?: string;
  project?: string;
  name?: string;
  environment?: string;
  port?: string;
  healthCheckPath?: string;
  timeout?: string;
}

/** The image reference as given, refusing what cannot be one. */
export function imageReference(
  value: string | undefined,
  option = "--image",
): string {
  const image = value?.trim() ?? "";
  if (image === "" || /\s/.test(image))
    throw new PluginCommandError(
      `Name the plugin's image with ${option}, for example ${option} registry.example.com/acme/reports:1.2.0.`,
    );
  return image;
}

/**
 * Where the plugin's image comes from: a registry the server pulls from, the
 * Docker on this machine, or a Dockerfile built here first.
 */
export type ImageSource =
  | { kind: "registry"; image: string }
  | { kind: "local"; image: string }
  | { kind: "build"; dockerfile: string; context: string };

/** The one source the options name, refusing none and more than one. */
export function imageSource(options: PluginImageOptions): ImageSource {
  const named = [
    options.image !== undefined,
    options.localImage !== undefined,
    options.build === true,
  ].filter(Boolean).length;
  if (named !== 1)
    throw new PluginCommandError(
      "Say where the plugin's image comes from with exactly one of: --image <image> (a registry the server pulls from), --local-image <image> (an image in your Docker) or --build (build the Dockerfile here).",
    );
  if (options.build) {
    const context = resolve(options.context ?? ".");
    const dockerfile = resolve(
      options.dockerfile ?? resolve(context, "Dockerfile"),
    );
    return { kind: "build", dockerfile, context };
  }
  if (options.dockerfile !== undefined || options.context !== undefined)
    throw new PluginCommandError(
      "--dockerfile and --context say what --build builds; pass --build with them.",
    );
  return options.localImage !== undefined
    ? {
        kind: "local",
        image: imageReference(options.localImage, "--local-image"),
      }
    : { kind: "registry", image: imageReference(options.image) };
}

/** Whether an image reference names exact content instead of a tag that can move. */
export function pinnedByDigest(image: string): boolean {
  return /@sha256:[0-9a-f]{64}$/.test(image);
}

/**
 * A service name for an image: the last part of its repository, without the
 * tag or digest. "registry.example.com/acme/reports:1.2.0" is "reports".
 */
export function serviceNameFor(image: string): string {
  const repository = image.replace(/@.*$/, "").replace(/:[^/:]*$/, "");
  const name = serviceSlug(repository.split("/").pop() ?? "");
  if (name === "")
    throw new PluginCommandError(
      `No service name can be taken from the image ${image}. Give one with --name.`,
    );
  return name;
}

function serviceSlug(value: string): string {
  return value
    .toLowerCase()
    .replace(/[^a-z0-9-]+/g, "-")
    .replace(/^-+|-+$/g, "");
}

/** The name of the service a source is deployed as, unless one is given. */
function defaultServiceName(source: ImageSource): string {
  if (source.kind !== "build") return serviceNameFor(source.image);
  const name = serviceSlug(basename(source.context));
  if (name === "")
    throw new PluginCommandError(
      `No service name can be taken from the directory ${source.context}. Give one with --name.`,
    );
  return name;
}

function port(value: string | undefined): number {
  if (value === undefined) return 8080;
  if (!/^[1-9][0-9]{0,4}$/.test(value) || Number(value) > 65535)
    throw new PluginCommandError(
      "--port is the port the plugin listens on, between 1 and 65535.",
    );
  return Number(value);
}

function healthCheckPath(value: string | undefined): string {
  const path = (value ?? "/health").trim();
  if (!path.startsWith("/"))
    throw new PluginCommandError(
      '--health-check-path must start with "/", for example /health.',
    );
  return path;
}

interface Service {
  id: number;
  name: string;
  slug: string;
}

/** The service named on the command line, or a new one made for the image. */
async function service(
  options: PluginImageOptions,
  defaultName: string,
): Promise<{ found: Service; created: boolean }> {
  if (options.project) {
    const result = /^\d+$/.test(options.project)
      ? await getProject({ client, path: { id: Number(options.project) } })
      : await getProjectBySlug({ client, path: { slug: options.project } });
    if (result.error || !result.data)
      throw new PluginCommandError(
        `No service ${options.project} was found. List services with temps projects list, or leave --project out to create one for the image.`,
      );
    return { found: result.data, created: false };
  }
  const name = options.name?.trim() || defaultName;
  // A plugin's image is never deployed over a service that merely shares
  // its name: that has to be asked for with --project.
  const existing = await getProjectBySlug({ client, path: { slug: name } });
  if (existing.data)
    throw new PluginCommandError(
      `A service named ${name} already exists. To deploy this image to it, pass --project ${name}; to create another service, pass --name.`,
    );
  const created = await createProject({
    client,
    body: {
      name,
      preset: "dockerfile",
      directory: "./",
      main_branch: "main",
      source_type: "docker_image",
      project_type: "docker",
      automatic_deploy: false,
      exposed_port: port(options.port),
    },
  });
  if (created.error || !created.data)
    throw new PluginCommandError(
      `The service ${name} could not be created: ${getErrorMessage(created.error)}`,
    );
  return { found: created.data, created: true };
}

async function environmentOf(
  found: Service,
  wanted: string,
): Promise<{ id: number; name: string }> {
  const { data } = await getEnvironments({
    client,
    path: { project_id: found.id },
  });
  const environments = normalizeEnvironmentsResponse(data);
  const environment = environments.find((entry) => entry.name === wanted);
  if (!environment)
    throw new PluginCommandError(
      `The service ${found.slug} has no environment named ${wanted}. It has: ${environments.map((entry) => entry.name).join(", ") || "none"}.`,
    );
  return { id: environment.id, name: environment.name };
}

/** The image a source is deployed as, building it here first when asked to. */
async function builtImage(
  source: ImageSource,
  options: PluginImageOptions,
  defaultName: string,
): Promise<string> {
  if (source.kind !== "build") return source.image;
  const image = `${serviceSlug(options.project ?? "") || defaultName || "plugin"}:plugin-${Date.now()}`;
  console.log(`Building ${image} from ${source.dockerfile}.`);
  try {
    await imageDeployment.build({
      dockerfile: source.dockerfile,
      context: source.context,
      tag: image,
      buildArgs: [],
      onOutput: (line) => console.log(`  ${line.trimEnd()}`),
    });
  } catch (error) {
    throw new PluginCommandError(
      `The image could not be built, so nothing was created or deployed: ${getErrorMessage(error)}`,
    );
  }
  return image;
}

/** Have the server pull an image from its registry and deploy it. */
async function pulled(
  found: Service,
  environmentId: number,
  image: string,
  health: string,
): Promise<{ id: number }> {
  if (!pinnedByDigest(image))
    console.log(
      "This image is named by a tag, which can be moved to other content. Name it by digest (repository@sha256:...) to deploy exactly what you reviewed.",
    );
  const started = await deployFromImage({
    client,
    path: { project_id: found.id, environment_id: environmentId },
    body: { image_ref: image, health_check_path: health },
  });
  if (started.error || !started.data)
    throw new PluginCommandError(
      `The image could not be deployed to ${found.slug}: ${getErrorMessage(started.error)}`,
    );
  return started.data;
}

/** Upload an image from the Docker on this machine and deploy it. */
async function uploaded(
  found: Service,
  environmentId: number,
  image: string,
  health: string,
  timeoutSecs: number,
): Promise<{ id: number }> {
  const started = await imageDeployment.upload({
    imageName: image,
    projectId: found.id,
    environmentId,
    healthCheckPath: health,
    timeoutSecs,
  });
  if (!started)
    throw new PluginCommandError(
      `The image ${image} was not uploaded to ${found.slug}, so nothing was deployed or added as a plugin. Run this again with --project ${found.slug} once the reason above is fixed.`,
    );
  return started;
}

async function servicePlugins(): Promise<ServicePlugin[]> {
  const result = await client.get<
    { 200: PluginServicesResponse },
    unknown,
    true
  >({ url: "/x/plugins/services", throwOnError: true });
  return result.data.plugins;
}

/**
 * Deploy an image as a service and make that service a plugin; for a service
 * that already is one, deploy the image as its update, which waits.
 */
async function addFromImage(options: PluginImageOptions) {
  const source = imageSource(options);
  const health = healthCheckPath(options.healthCheckPath);
  const timeoutSecs = Number.parseInt(options.timeout ?? "300", 10);
  if (!Number.isInteger(timeoutSecs) || timeoutSecs < 1)
    throw new PluginCommandError("--timeout is a number of seconds.");
  // Refused before anything is created: a service is never left behind for
  // an image that is not there.
  const defaultName = options.project ? "" : defaultServiceName(source);
  if (source.kind === "build" && !existsSync(source.dockerfile))
    throw new PluginCommandError(
      `No Dockerfile at ${source.dockerfile}. Run this in the plugin's directory, or name it with --dockerfile and --context.`,
    );
  await requireAuth();
  await setupClient();
  try {
    if (
      source.kind === "local" &&
      !(await imageDeployment.exists(source.image))
    )
      throw new PluginCommandError(
        `Your Docker has no image ${source.image}. List what it has with docker images, build the plugin here with --build, or name a registry image with --image.`,
      );
    const image = await builtImage(source, options, defaultName);
    const { found, created } = await service(options, defaultName);
    const environment = await environmentOf(
      found,
      options.environment ?? "production",
    );
    if (created) console.log(`Created the service ${found.slug}.`);
    const deployment =
      source.kind === "registry"
        ? await pulled(found, environment.id, image, health)
        : await uploaded(found, environment.id, image, health, timeoutSecs);
    const deployed = await imageDeployment.watch({
      projectId: found.id,
      deploymentId: deployment.id,
      timeoutSecs,
      projectName: found.slug,
    });
    if (!deployed.success)
      throw new PluginCommandError(
        `Deployment ${deployment.id} of ${found.slug} did not become live${deployed.error ? `: ${deployed.error}` : ""}. Nothing was added as a plugin. See temps deployments logs, fix the image and run this again with --project ${found.slug}.`,
      );

    const bound = (plugin: ServicePlugin) =>
      plugin.project_id === found.id &&
      plugin.environment_id === environment.id;
    const existing = (await servicePlugins()).find(bound);
    if (!existing) {
      const added = await client.post<
        { 201: AddedServicePlugin },
        unknown,
        true
      >({
        url: "/x/plugins/services",
        headers: { "Content-Type": "application/json" },
        body: { project_id: found.id, environment_id: environment.id },
        throwOnError: true,
      });
      console.log(
        `Plugin ${added.data.name} (version ${added.data.version}) was added from ${image} and is pinned to deployment ${deployment.id}.`,
      );
      console.log(
        `It has no permissions yet. See what it asks for with temps plugin grants get ${added.data.name}.`,
      );
      return;
    }

    // The service already is a plugin: the image is an update, and an
    // update never runs as the plugin until it is approved.
    for (let lookup = 0; lookup < UPDATE_LOOKUPS; lookup++) {
      const plugin = (await servicePlugins()).find(bound);
      if (plugin?.pending_update?.deployment_id === deployment.id) {
        for (const line of updateLines(plugin)) console.log(line);
        return;
      }
      await new Promise((resolve) =>
        setTimeout(resolve, lookup === 0 ? 0 : 1000),
      );
    }
    console.log(
      `Deployment ${deployment.id} is live. Plugin ${existing.name} stays on the build that was approved until you approve this one: temps plugin update ${existing.name}`,
    );
  } catch (error) {
    if (error instanceof PluginCommandError) throw error;
    throw new PluginCommandError(getErrorMessage(error));
  }
}

export function registerPluginImageCommands(plugin: Command) {
  plugin
    .command("add")
    .description(
      "Deploy a plugin's image as a service and add it as a plugin, in one step; for a plugin that is already added, deploy the image as an update that waits for approval. The image comes from a registry (--image), from your Docker (--local-image) or from a Dockerfile built here (--build)",
    )
    .option(
      "--image <image>",
      "Image of the plugin in a registry the server pulls from, by digest or tag (e.g. registry.example.com/acme/reports:1.2.0)",
    )
    .option(
      "--local-image <image>",
      "Image of the plugin in your Docker; it is uploaded to the server, no registry needed",
    )
    .option(
      "--build",
      "Build the plugin's Dockerfile here, then upload the image to the server",
    )
    .option(
      "--dockerfile <path>",
      "Dockerfile that --build builds (default: Dockerfile in the context)",
    )
    .option(
      "--context <path>",
      "Directory that --build builds from (default: the current directory)",
    )
    .option(
      "-p, --project <project>",
      "Slug or id of an existing service to deploy the image to; without it a new service is created",
    )
    .option(
      "--name <name>",
      "Name of the service to create (default: taken from the image, or from the directory that is built)",
    )
    .option("-e, --environment <env>", "Environment name", "production")
    .option(
      "--port <port>",
      "Port the plugin listens on, for a service that is created",
      "8080",
    )
    .option(
      "--health-check-path <path>",
      "Path the deployment is checked on; a plugin answers /health unless its manifest says otherwise",
      "/health",
    )
    .option(
      "--timeout <seconds>",
      "Seconds to wait for the deployment to be live",
      "300",
    )
    .action(addFromImage);
}
