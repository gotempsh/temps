# Docker Compose security exceptions

Compose projects enforce their security checks by default. Instance administrators can grant individual exceptions under **Settings → Build & deploy → Build → Docker Compose → Advanced security settings**. The section starts collapsed and explains that it is intended for experienced administrators running trusted stacks.

Each disabled check requires an acknowledgment of its consequences. Changes apply on the next deployment, not to running containers. Other checks remain enforced. Ordinary project-scoped named volumes need no exception.

## Sharing certificates between stacks

The consuming project must disable **Block external volumes** and, when using `name`, **Block custom volume names**:

```yaml
services:
  proxy:
    image: nginx:alpine
    volumes:
      - certificates:/etc/certificates:ro

volumes:
  certificates:
    external: true
    name: shared-certificates
```

The volume must already exist on the same Docker daemon. The producer can use an explicit `name: shared-certificates` with the custom-name exception, or the consumer can reference the producer’s existing Docker volume name. The read-only consumer mount prevents it from modifying certificates.

## Compatibility

These settings replace the former `unsandboxedServices` option. Existing deployments keep running, but the next deployment enforces the new policy. Administrators must acknowledge any exceptions they still need before redeploying. Generic project updates and templates cannot grant legacy sandbox exceptions. A durable migration notice survives unrelated project edits and policy toggles. After reviewing the required exceptions, an administrator can explicitly complete the migration with `acknowledge_legacy_migration: true`; the notice is cleared in the same audited transaction.

## API and enforcement

- `GET /api/projects/{id}/compose-security` returns the policy, check catalog, and `can_edit`.
- `PUT /api/projects/{id}/compose-security` accepts `policy.disabled_checks`, the required `expected_policy` snapshot, and `acknowledge_risks`. Newly disabled checks require acknowledgment; restoring checks does not. The server compares `expected_policy` with the current policy while holding the project lock and returns `409 Conflict` on a mismatch. Refresh and review before retrying; stale requests never restore revoked exceptions.
- Only effective instance-administrator credentials with project write access can change grants. Deployment tokens and project-writer API keys cannot.
- Policy changes and their actor/previous state are committed atomically in an audit-history transaction. Grants are stored separately from imported project configuration.
- Referenced Compose files are resolved and the resulting configuration is checked before deployment. Allowing `extends` or `include` does not waive checks on inherited fields.
- Valid Compose syntax, bounded reference resolution, repository confinement of referenced Compose files, and protection of Temps-generated writes remain required. Exceptions do not grant arbitrary writes by Temps itself.

## Available checks

| Group | Check | When disabled |
|---|---|---|
| Composition | **Block extends** (`extends`) | Reuse service definitions from other Compose files. |
| Composition | **Block include** (`include`) | Load additional Compose files and their services. |
| Composition | **Block variables in guarded fields** (`interpolation`) | Resolve variables in security-sensitive settings before validating their values. |
| Composition | **Block new services in inline overrides** (`inline_services`) | Add services through the inline Compose override. |
| Composition | **Block top-level inline override sections** (`inline_sections`) | Add networks, volumes, configs, and secrets through an override. |
| Composition | **Block restricted inline override fields** (`inline_fields`) | Set restricted fields in overrides; their individual policies still apply. |
| Runtime | **Block privileged containers** (`privileged`) | Give containers privileged access to the Docker host. |
| Runtime | **Block Docker Engine access** (`docker_socket`) | Mount the Docker socket or use use_api_socket to control the host daemon. |
| Runtime | **Block additional capabilities** (`capabilities`) | Grant custom Linux capabilities with cap_add. |
| Runtime | **Drop Linux capabilities** (`drop_capabilities`) | Restore Docker default capabilities instead of the Temps restricted set. |
| Runtime | **Block custom security options** (`security_options`) | Customize seccomp, AppArmor, and other security_opt settings. |
| Runtime | **Prevent privilege escalation** (`no_new_privileges`) | Allow privilege gains through executable files inside containers. |
| Runtime | **Block host devices** (`devices`) | Map host devices into containers. |
| Runtime | **Block device cgroup rules** (`device_rules`) | Configure device access rules. |
| Runtime | **Block GPU access** (`gpu`) | Expose GPUs and reserved devices to containers. |
| Runtime | **Block kernel parameters** (`sysctls`) | Set namespaced kernel parameters through sysctls. |
| Runtime | **Block supplementary groups** (`groups`) | Add supplementary groups through group_add. |
| Runtime | **Block custom cgroup placement** (`cgroup_parent`) | Place containers under a custom cgroup parent. |
| Runtime | **Block alternative runtimes** (`runtime`) | Select another installed OCI runtime. |
| Runtime | **Block lifecycle hooks** (`lifecycle_hooks`) | Run post_start and pre_stop hooks, including privileged hooks. |
| Runtime | **Block Compose providers** (`provider`) | Invoke installed Compose provider plugins. |
| Runtime | **Block custom container names** (`container_name`) | Use daemon-global container names that may collide with other projects. |
| Runtime | **Inject Docker init** (`init`) | Let the Compose file and image control init behavior. |
| Networking | **Block host networking** (`host_network`) | Share the host network namespace. |
| Networking | **Block host PID namespace** (`host_pid`) | Share the host process namespace. |
| Networking | **Block host IPC namespace** (`host_ipc`) | Share the host IPC namespace. |
| Networking | **Block host UTS namespace** (`host_uts`) | Share the host hostname namespace. |
| Networking | **Block host cgroup namespace** (`host_cgroup`) | Share the host cgroup namespace. |
| Networking | **Block host user namespace** (`host_user`) | Use the host user namespace. |
| Networking | **Block other-container namespaces** (`container_namespace`) | Join namespaces of arbitrary existing containers. |
| Networking | **Restrict other network modes** (`network_mode`) | Use network modes outside project networks, none, and declared services. |
| Networking | **Block external networks** (`external_networks`) | Connect to existing Docker networks. |
| Networking | **Block custom network names** (`network_names`) | Use daemon-global Docker network names. |
| Networking | **Restrict network drivers to bridge** (`network_drivers`) | Use other installed network drivers. |
| Networking | **Block network driver options** (`network_options`) | Change network driver options affecting host networking. |
| Networking | **Block custom IPAM** (`network_ipam`) | Configure network address allocation and routing. |
| Networking | **Block external container links** (`external_links`) | Connect to containers outside this project. |
| Networking | **Require loopback port bindings** (`published_ports`) | Publish ports on other host addresses, bypassing the Temps proxy. |
| Storage | **Confine bind mounts to the project** (`bind_mounts`) | Mount absolute host paths or paths outside the project. |
| Storage | **Block custom volume drivers** (`volume_drivers`) | Use non-local volume drivers. |
| Storage | **Block network filesystem volumes** (`volume_network_filesystems`) | Mount NFS, CIFS, and other network filesystems through volume options. |
| Storage | **Confine named-volume host paths** (`volume_host_paths`) | Use host paths through named-volume driver options. |
| Storage | **Block other volume driver options** (`volume_options`) | Pass custom options to volume drivers. |
| Storage | **Block external volumes** (`external_volumes`) | Attach existing Docker volumes, including other projects' data. |
| Storage | **Block custom volume names** (`volume_names`) | Use daemon-global volume names. |
| Storage | **Block inherited container volumes** (`volumes_from`) | Inherit mounts from another container with volumes_from. |
| Storage | **Confine config file paths** (`config_paths`) | Read Compose configs from outside the project. |
| Storage | **Confine secret file paths** (`secret_paths`) | Read Compose secrets from outside the project. |
| Storage | **Block external configs** (`external_configs`) | Reference daemon-global or externally named configs. |
| Storage | **Block external secrets** (`external_secrets`) | Reference daemon-global or externally named secrets. |
| Storage | **Confine environment file paths** (`env_files`) | Read existing environment files outside the project; Temps never writes there. |
| Storage | **Block label files** (`label_files`) | Read container labels from files on the deployment host. |
| Storage | **Block storage options** (`storage_options`) | Customize Docker storage driver options. |
| Resources | **Protect OOM-killer behavior** (`oom_killer`) | Disable the container OOM killer. |
| Resources | **Limit service shared memory** (`service_shm`) | Configure more than 512 MiB shared memory per service. |
| Resources | **Limit aggregate shared memory** (`aggregate_shm`) | Configure more than 1 GiB total shared memory per stack. |
| Resources | **Block memory-backed mounts** (`tmpfs`) | Create tmpfs mounts. |
| Resources | **Block custom resource limits** (`ulimits`) | Set container ulimits. |
| Resources | **Enforce PID limit** (`pids`) | Remove the injected 512-process limit. |
| Resources | **Enforce memory limit** (`memory`) | Remove the injected 4 GiB memory limit. |
| Resources | **Enforce bounded Docker logs** (`logging`) | Use custom logging settings instead of Temps log rotation. |
| Resources | **Block block-device I/O options** (`blkio`) | Configure blkio_config scheduling. |
| Resources | **Block custom swap limits** (`swap`) | Configure memswap_limit. |
| Resources | **Restrict replicas** (`replicas`) | Use custom scale, replica counts, or deployment modes. |
| Build | **Block remote build contexts** (`remote_build`) | Fetch Git or URL build contexts. |
| Build | **Confine build context paths** (`build_context`) | Read build contexts outside the project. |
| Build | **Confine Dockerfile paths** (`dockerfile`) | Read Dockerfiles outside the project. |
| Build | **Block privileged builds** (`build_privileged`) | Run privileged build steps. |
| Build | **Block build entitlements** (`build_entitlements`) | Grant build entitlements such as security.insecure. |
| Build | **Restrict build networking** (`build_network`) | Use host or named networks during builds. |
| Build | **Block build SSH forwarding** (`build_ssh`) | Forward configured SSH agents or keys into builds. |
| Build | **Block build shared-memory overrides** (`build_shm`) | Set build.shm_size. |
| Build | **Block build resource-limit overrides** (`build_ulimits`) | Set build.ulimits. |
| Build | **Block additional build contexts** (`build_additional_contexts`) | Read additional local or remote build contexts. |
| Build | **Block build cache imports** (`build_cache_from`) | Import build cache from external locations. |
| Build | **Block build cache exports** (`build_cache_to`) | Export build cache to local or external locations. |
| Build | **Block additional build tags** (`build_tags`) | Assign additional daemon-global image tags. |
| Build | **Restrict local image references** (`image_references`) | Use raw image IDs or Temps-internal images. |
| Build | **Block custom build image names** (`build_image`) | Assign an explicit image tag to a build. |
| Build | **Enforce registry pulls** (`pull_policy`) | Use custom pull policies instead of always pulling image services. |
