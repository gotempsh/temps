// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What the launcher accepts from the server, and what it turns that into.
//!
//! The server writes `vms/<name>/vm.json` as an unprivileged user, so the
//! launcher (root) treats it as hostile input: it parses it into a closed
//! set of fields, refuses everything else, and renders its own config for
//! the jail. Nothing here touches the disk; [`super::server`] does that.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::LauncherConfig;

/// The only file name a drive may use: the VM's own disk, in its directory.
pub const ROOTFS_FILE: &str = "rootfs.ext4";
/// The guest's vsock socket inside the jail, which the server connects to.
pub const VSOCK_FILE: &str = "v.sock";
/// Where the pinned kernel is placed inside the jail.
pub const KERNEL_FILE: &str = "vmlinux";
/// The rendered config inside the jail.
pub const CONFIG_FILE: &str = "vm.json";

const TAP_PREFIX: &str = "temps-fc-tap";
const MAX_BOOT_ARGS_BYTES: usize = 4096;
const MAX_VCPUS: u64 = 32;
const MIN_MEM_MIB: u64 = 16;
const MAX_MEM_MIB: u64 = 512 * 1024;
const MAX_VM_NAME_BYTES: usize = 120;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PlanError {
    #[error("VM name '{name}' is not allowed: {reason}")]
    VmName { name: String, reason: &'static str },
    #[error("VM '{vm}' config is not valid JSON: {reason}")]
    NotJson { vm: String, reason: String },
    #[error("VM '{vm}' config: {field}: {reason}")]
    Field {
        vm: String,
        field: String,
        reason: String,
    },
}

/// A VM name becomes a path component and a jail id; keep it plain.
pub fn validate_vm_name(name: &str) -> Result<(), PlanError> {
    let bad = |reason: &'static str| PlanError::VmName {
        name: name.to_string(),
        reason,
    };
    if name.is_empty() {
        return Err(bad("it is empty"));
    }
    if name.len() > MAX_VM_NAME_BYTES {
        return Err(bad("it is too long"));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(bad("it may hold only letters, digits, '-', '_' and '.'"));
    }
    if name.starts_with('.') || name.starts_with('-') {
        return Err(bad("it may not start with '.' or '-'"));
    }
    Ok(())
}

/// One network interface of the VM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IfaceSpec {
    pub iface_id: String,
    pub guest_mac: String,
    /// The TAP device, `temps-fc-tap<index>`.
    pub tap: String,
    pub rx_rate_limiter: Option<Value>,
    pub tx_rate_limiter: Option<Value>,
}

/// A VM as the launcher is willing to run it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmSpec {
    /// The kernel the server asked for; the launcher accepts it only from its
    /// own kernel directory.
    pub kernel_path: PathBuf,
    pub boot_args: String,
    pub vcpus: u32,
    pub mem_mib: u64,
    pub drive_read_only: bool,
    pub drive_rate_limiter: Option<Value>,
    pub interfaces: Vec<IfaceSpec>,
}

fn field_error(vm: &str, field: &str, reason: impl Into<String>) -> PlanError {
    PlanError::Field {
        vm: vm.to_string(),
        field: field.to_string(),
        reason: reason.into(),
    }
}

fn expect_object<'a>(
    vm: &str,
    field: &str,
    value: &'a Value,
) -> Result<&'a serde_json::Map<String, Value>, PlanError> {
    value
        .as_object()
        .ok_or_else(|| field_error(vm, field, "must be an object"))
}

/// Refuse any key outside `allowed`, naming it: a field the launcher does not
/// know is one it cannot vouch for.
fn only_keys(
    vm: &str,
    field: &str,
    object: &serde_json::Map<String, Value>,
    allowed: &[&str],
) -> Result<(), PlanError> {
    for key in object.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(field_error(
                vm,
                &format!("{field}.{key}"),
                "this field is not permitted in a jailed VM",
            ));
        }
    }
    Ok(())
}

fn rate_limiter(vm: &str, field: &str, value: &Value) -> Result<Value, PlanError> {
    let object = expect_object(vm, field, value)?;
    only_keys(vm, field, object, &["bandwidth", "ops"])?;
    let mut clean = serde_json::Map::new();
    for (name, bucket) in object {
        let path = format!("{field}.{name}");
        let bucket_object = expect_object(vm, &path, bucket)?;
        only_keys(
            vm,
            &path,
            bucket_object,
            &["size", "one_time_burst", "refill_time"],
        )?;
        let mut clean_bucket = serde_json::Map::new();
        for (key, number) in bucket_object {
            let n = number
                .as_u64()
                .ok_or_else(|| field_error(vm, &format!("{path}.{key}"), "must be a number"))?;
            if n == 0 && key != "one_time_burst" {
                return Err(field_error(vm, &format!("{path}.{key}"), "must be positive"));
            }
            clean_bucket.insert(key.clone(), json!(n));
        }
        if !clean_bucket.contains_key("size") || !clean_bucket.contains_key("refill_time") {
            return Err(field_error(vm, &path, "needs 'size' and 'refill_time'"));
        }
        clean.insert(name.clone(), Value::Object(clean_bucket));
    }
    Ok(Value::Object(clean))
}

fn tap_name(vm: &str, field: &str, value: &Value) -> Result<String, PlanError> {
    let name = value
        .as_str()
        .ok_or_else(|| field_error(vm, field, "must be a string"))?;
    let index = name
        .strip_prefix(TAP_PREFIX)
        .filter(|rest| !rest.is_empty() && rest.len() <= 6)
        .filter(|rest| rest.chars().all(|c| c.is_ascii_digit()));
    match index {
        Some(_) => Ok(name.to_string()),
        None => Err(field_error(
            vm,
            field,
            format!("must be a pool device named {TAP_PREFIX}<number>"),
        )),
    }
}

fn mac_address(vm: &str, field: &str, value: &Value) -> Result<String, PlanError> {
    let mac = value
        .as_str()
        .ok_or_else(|| field_error(vm, field, "must be a string"))?;
    let octets: Vec<&str> = mac.split(':').collect();
    let valid = octets.len() == 6
        && octets
            .iter()
            .all(|o| o.len() == 2 && o.chars().all(|c| c.is_ascii_hexdigit()));
    if valid {
        Ok(mac.to_string())
    } else {
        Err(field_error(vm, field, "must be six colon-separated hex octets"))
    }
}

/// Parse the server's `vm.json`. Kernel, disk and socket locations are not
/// read from it at all: the launcher decides those.
pub fn parse_vm_config(vm: &str, raw: &[u8]) -> Result<VmSpec, PlanError> {
    let document: Value = serde_json::from_slice(raw).map_err(|e| PlanError::NotJson {
        vm: vm.to_string(),
        reason: e.to_string(),
    })?;
    let root = expect_object(vm, "$", &document)?;
    only_keys(
        vm,
        "$",
        root,
        &[
            "boot-source",
            "drives",
            "machine-config",
            "vsock",
            "network-interfaces",
        ],
    )?;

    let boot = expect_object(vm, "boot-source", root.get("boot-source").unwrap_or(&Value::Null))?;
    only_keys(vm, "boot-source", boot, &["kernel_image_path", "boot_args"])?;
    let boot_args = boot
        .get("boot_args")
        .and_then(Value::as_str)
        .ok_or_else(|| field_error(vm, "boot-source.boot_args", "must be a string"))?;
    if boot_args.len() > MAX_BOOT_ARGS_BYTES || boot_args.chars().any(|c| c.is_control()) {
        return Err(field_error(
            vm,
            "boot-source.boot_args",
            "must be printable and at most 4096 bytes",
        ));
    }

    let machine = expect_object(
        vm,
        "machine-config",
        root.get("machine-config").unwrap_or(&Value::Null),
    )?;
    only_keys(vm, "machine-config", machine, &["vcpu_count", "mem_size_mib"])?;
    let vcpus = machine
        .get("vcpu_count")
        .and_then(Value::as_u64)
        .filter(|v| (1..=MAX_VCPUS).contains(v))
        .ok_or_else(|| field_error(vm, "machine-config.vcpu_count", "must be 1 to 32"))?;
    let mem_mib = machine
        .get("mem_size_mib")
        .and_then(Value::as_u64)
        .filter(|v| (MIN_MEM_MIB..=MAX_MEM_MIB).contains(v))
        .ok_or_else(|| {
            field_error(vm, "machine-config.mem_size_mib", "must be 16 to 524288")
        })?;

    let drives = root
        .get("drives")
        .and_then(Value::as_array)
        .ok_or_else(|| field_error(vm, "drives", "must be a list"))?;
    let [drive] = drives.as_slice() else {
        return Err(field_error(vm, "drives", "must hold exactly the root disk"));
    };
    let drive = expect_object(vm, "drives[0]", drive)?;
    only_keys(
        vm,
        "drives[0]",
        drive,
        &[
            "drive_id",
            "path_on_host",
            "is_root_device",
            "is_read_only",
            "rate_limiter",
        ],
    )?;
    if drive.get("path_on_host").and_then(Value::as_str) != Some(ROOTFS_FILE) {
        return Err(field_error(
            vm,
            "drives[0].path_on_host",
            format!("must be '{ROOTFS_FILE}'"),
        ));
    }
    if drive.get("is_root_device").and_then(Value::as_bool) != Some(true) {
        return Err(field_error(vm, "drives[0].is_root_device", "must be true"));
    }
    let drive_rate_limiter = drive
        .get("rate_limiter")
        .map(|v| rate_limiter(vm, "drives[0].rate_limiter", v))
        .transpose()?;

    if let Some(vsock) = root.get("vsock") {
        let vsock = expect_object(vm, "vsock", vsock)?;
        only_keys(vm, "vsock", vsock, &["guest_cid", "uds_path"])?;
        if vsock.get("uds_path").and_then(Value::as_str) != Some(VSOCK_FILE) {
            return Err(field_error(
                vm,
                "vsock.uds_path",
                format!("must be '{VSOCK_FILE}'"),
            ));
        }
        if vsock.get("guest_cid").and_then(Value::as_u64) != Some(3) {
            return Err(field_error(vm, "vsock.guest_cid", "must be 3"));
        }
    } else {
        return Err(field_error(vm, "vsock", "is required: the server talks to the guest over it"));
    }

    let mut interfaces = Vec::new();
    let listed = root
        .get("network-interfaces")
        .and_then(Value::as_array)
        .ok_or_else(|| field_error(vm, "network-interfaces", "must be a list"))?;
    for (i, entry) in listed.iter().enumerate() {
        let path = format!("network-interfaces[{i}]");
        let entry = expect_object(vm, &path, entry)?;
        only_keys(
            vm,
            &path,
            entry,
            &[
                "iface_id",
                "guest_mac",
                "host_dev_name",
                "rx_rate_limiter",
                "tx_rate_limiter",
            ],
        )?;
        let limiter = |key: &str| {
            entry
                .get(key)
                .map(|v| rate_limiter(vm, &format!("{path}.{key}"), v))
                .transpose()
        };
        interfaces.push(IfaceSpec {
            iface_id: entry
                .get("iface_id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty() && s.len() <= 16 && s.chars().all(|c| c.is_ascii_alphanumeric()))
                .ok_or_else(|| field_error(vm, &format!("{path}.iface_id"), "must be a short alphanumeric id"))?
                .to_string(),
            guest_mac: mac_address(
                vm,
                &format!("{path}.guest_mac"),
                entry.get("guest_mac").unwrap_or(&Value::Null),
            )?,
            tap: tap_name(
                vm,
                &format!("{path}.host_dev_name"),
                entry.get("host_dev_name").unwrap_or(&Value::Null),
            )?,
            rx_rate_limiter: limiter("rx_rate_limiter")?,
            tx_rate_limiter: limiter("tx_rate_limiter")?,
        });
    }
    if interfaces.len() > 1 {
        return Err(field_error(
            vm,
            "network-interfaces",
            "a jailed VM has at most one interface",
        ));
    }

    let kernel_path = boot
        .get("kernel_image_path")
        .and_then(Value::as_str)
        .filter(|p| !p.is_empty() && p.len() <= 4096 && !p.contains('\0'))
        .map(PathBuf::from)
        .ok_or_else(|| field_error(vm, "boot-source.kernel_image_path", "must be a path"))?;

    Ok(VmSpec {
        kernel_path,
        boot_args: boot_args.to_string(),
        vcpus: vcpus as u32,
        mem_mib,
        drive_read_only: drive
            .get("is_read_only")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        drive_rate_limiter,
        interfaces,
    })
}

/// The config the VMM reads inside the jail: the validated fields with the
/// launcher's own file names.
pub fn render_jail_config(spec: &VmSpec) -> Value {
    let mut drive = json!({
        "drive_id": "rootfs",
        "path_on_host": ROOTFS_FILE,
        "is_root_device": true,
        "is_read_only": spec.drive_read_only,
    });
    if let Some(limiter) = &spec.drive_rate_limiter {
        drive["rate_limiter"] = limiter.clone();
    }
    let interfaces: Vec<Value> = spec
        .interfaces
        .iter()
        .map(|iface| {
            let mut value = json!({
                "iface_id": iface.iface_id,
                "guest_mac": iface.guest_mac,
                "host_dev_name": iface.tap,
            });
            if let Some(limiter) = &iface.rx_rate_limiter {
                value["rx_rate_limiter"] = limiter.clone();
            }
            if let Some(limiter) = &iface.tx_rate_limiter {
                value["tx_rate_limiter"] = limiter.clone();
            }
            value
        })
        .collect();
    json!({
        "boot-source": {
            "kernel_image_path": KERNEL_FILE,
            "boot_args": spec.boot_args,
        },
        "drives": [drive],
        "machine-config": { "vcpu_count": spec.vcpus, "mem_size_mib": spec.mem_mib },
        "vsock": { "guest_cid": 3, "uds_path": VSOCK_FILE },
        "network-interfaces": interfaces,
    })
}

/// The jailer's command line for `vm`: the identity it drops to, where the
/// chroots live, and the VMM's own arguments after `--`.
pub fn jailer_args(config: &LauncherConfig, jail_id: &str, uid: u32, gid: u32) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        "--id".into(),
        jail_id.into(),
        "--exec-file".into(),
        config.firecracker_bin.clone().into_os_string(),
        "--uid".into(),
        uid.to_string().into(),
        "--gid".into(),
        gid.to_string().into(),
        "--chroot-base-dir".into(),
        config.jail_base.clone().into_os_string(),
        "--".into(),
    ];
    args.extend(
        ["--no-api", "--config-file", CONFIG_FILE]
            .into_iter()
            .map(OsString::from),
    );
    args
}

/// Per-VM identity slots: each VM keeps the same uid for as long as it exists,
/// so the ownership of its disk stays valid across restarts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotTable {
    slots: BTreeMap<String, u32>,
}

impl SlotTable {
    /// The VM's slot, assigning the lowest free one if it has none. `None`
    /// when all `capacity` slots belong to other VMs.
    pub fn slot_for(&mut self, vm: &str, capacity: u32) -> Option<u32> {
        if let Some(slot) = self.slots.get(vm) {
            return Some(*slot);
        }
        let taken: std::collections::BTreeSet<u32> = self.slots.values().copied().collect();
        let free = (0..capacity).find(|slot| !taken.contains(slot))?;
        self.slots.insert(vm.to_string(), free);
        Some(free)
    }

    pub fn release(&mut self, vm: &str) -> bool {
        self.slots.remove(vm).is_some()
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }
}

/// The uid and gid of a slot, checked against the configured range.
pub fn identity_for_slot(config: &LauncherConfig, slot: u32) -> Option<(u32, u32)> {
    (slot < config.uid_count).then(|| {
        let id = config.uid_base + slot;
        (id, id)
    })
}

/// Whether `path` lies under `root` once both are absolute and free of `..`.
pub fn is_within(root: &Path, path: &Path) -> bool {
    path.is_absolute()
        && !path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        && path.starts_with(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> LauncherConfig {
        LauncherConfig {
            fc_root: PathBuf::from("/data/firecracker"),
            jail_base: PathBuf::from("/data/firecracker/jail"),
            firecracker_bin: PathBuf::from("/data/firecracker/bin/firecracker"),
            jailer_bin: PathBuf::from("/data/firecracker/bin/jailer"),
            kernel_dir: PathBuf::from("/data/firecracker/kernel"),
            server_uid: 1000,
            server_gid: 1000,
            uid_base: 52000,
            uid_count: 3,
            bridge: "temps-fc-br0".into(),
            socket: PathBuf::from("/run/temps-fc/launcher.sock"),
        }
    }

    /// What the provider writes today.
    fn server_config() -> Value {
        json!({
            "boot-source": {
                "kernel_image_path": "/data/firecracker/kernel/vmlinux-6.1.141",
                "boot_args": "console=ttyS0 reboot=k panic=1 pci=off ip=192.168.222.10::192.168.222.1:255.255.255.0::eth0:off",
            },
            "drives": [{
                "drive_id": "rootfs",
                "path_on_host": "rootfs.ext4",
                "is_root_device": true,
                "is_read_only": false,
            }],
            "machine-config": { "vcpu_count": 2, "mem_size_mib": 512 },
            "vsock": { "guest_cid": 3, "uds_path": "v.sock" },
            "network-interfaces": [{
                "iface_id": "eth0",
                "guest_mac": "06:fc:c0:a8:de:0a",
                "host_dev_name": "temps-fc-tap0",
            }],
        })
    }

    fn parse(value: &Value) -> Result<VmSpec, PlanError> {
        parse_vm_config("vm1", value.to_string().as_bytes())
    }

    #[test]
    fn what_the_provider_writes_today_is_accepted() {
        let spec = parse(&server_config()).expect("accepted");
        assert_eq!((spec.vcpus, spec.mem_mib), (2, 512));
        assert_eq!(spec.interfaces[0].tap, "temps-fc-tap0");
        assert!(spec.boot_args.contains("console=ttyS0"));
    }

    #[test]
    fn the_jail_config_names_the_launchers_files_not_the_servers() {
        let spec = parse(&server_config()).unwrap();
        let rendered = render_jail_config(&spec);
        assert_eq!(rendered["boot-source"]["kernel_image_path"], KERNEL_FILE);
        assert_eq!(rendered["drives"][0]["path_on_host"], ROOTFS_FILE);
        assert_eq!(rendered["vsock"]["uds_path"], VSOCK_FILE);
        assert!(
            !rendered.to_string().contains("/data/firecracker"),
            "no host path survives into the jail: {rendered}"
        );
        // Rendering what was parsed is stable, apart from the kernel, which
        // now names the jail's own copy.
        let reparsed = parse_vm_config("vm1", rendered.to_string().as_bytes()).unwrap();
        assert_eq!(reparsed.kernel_path, PathBuf::from(KERNEL_FILE));
        assert_eq!(
            VmSpec {
                kernel_path: spec.kernel_path.clone(),
                ..reparsed
            },
            spec
        );
    }

    #[test]
    fn rate_limiters_survive_and_are_validated() {
        let mut config = server_config();
        let limiter = json!({"bandwidth": {"size": 1048576, "refill_time": 100}});
        config["drives"][0]["rate_limiter"] = limiter.clone();
        config["network-interfaces"][0]["rx_rate_limiter"] = limiter.clone();
        config["network-interfaces"][0]["tx_rate_limiter"] = limiter.clone();
        let spec = parse(&config).unwrap();
        let rendered = render_jail_config(&spec);
        assert_eq!(rendered["drives"][0]["rate_limiter"], limiter);
        assert_eq!(rendered["network-interfaces"][0]["tx_rate_limiter"], limiter);

        config["drives"][0]["rate_limiter"] = json!({"bandwidth": {"size": 0, "refill_time": 100}});
        assert!(parse(&config).unwrap_err().to_string().contains("must be positive"));
        config["drives"][0]["rate_limiter"] = json!({"bandwidth": {"size": 1}});
        assert!(parse(&config).unwrap_err().to_string().contains("'refill_time'"));
        config["drives"][0]["rate_limiter"] = json!({"evil": {"size": 1, "refill_time": 1}});
        assert!(parse(&config).unwrap_err().to_string().contains("evil"));
    }

    fn rejects(mutate: impl FnOnce(&mut Value), fragment: &str) {
        let mut config = server_config();
        mutate(&mut config);
        let message = parse(&config).unwrap_err().to_string();
        assert!(message.contains(fragment), "{message}");
        assert!(message.contains("vm1"), "names the VM: {message}");
    }

    #[test]
    fn anything_that_points_the_vmm_outside_its_jail_is_refused() {
        // A second disk, a path to some other file, a logger or metrics
        // sink (a write to any path), a balloon, an unknown top-level key.
        rejects(
            |c| c["drives"][0]["path_on_host"] = json!("/etc/shadow"),
            "drives[0].path_on_host",
        );
        rejects(
            |c| c["drives"][0]["path_on_host"] = json!("../other/rootfs.ext4"),
            "drives[0].path_on_host",
        );
        rejects(
            |c| c["drives"].as_array_mut().unwrap().push(json!({"drive_id": "x"})),
            "exactly the root disk",
        );
        rejects(
            |c| c["logger"] = json!({"log_path": "/root/.ssh/authorized_keys"}),
            "$.logger",
        );
        rejects(|c| c["metrics"] = json!({"metrics_path": "/x"}), "$.metrics");
        rejects(|c| c["balloon"] = json!({}), "$.balloon");
        rejects(
            |c| c["vsock"]["uds_path"] = json!("/tmp/v.sock"),
            "vsock.uds_path",
        );
        rejects(
            |c| c["drives"][0]["cache_type"] = json!("Writeback"),
            "cache_type",
        );
    }

    #[test]
    fn network_devices_must_come_from_the_tap_pool() {
        rejects(
            |c| c["network-interfaces"][0]["host_dev_name"] = json!("eth0"),
            "temps-fc-tap<number>",
        );
        rejects(
            |c| c["network-interfaces"][0]["host_dev_name"] = json!("temps-fc-tap0; reboot"),
            "temps-fc-tap<number>",
        );
        rejects(
            |c| c["network-interfaces"][0]["guest_mac"] = json!("not-a-mac"),
            "hex octets",
        );
        rejects(
            |c| {
                let second = c["network-interfaces"][0].clone();
                c["network-interfaces"].as_array_mut().unwrap().push(second);
            },
            "at most one interface",
        );
    }

    #[test]
    fn machine_size_and_boot_args_are_bounded() {
        rejects(|c| c["machine-config"]["vcpu_count"] = json!(0), "vcpu_count");
        rejects(|c| c["machine-config"]["vcpu_count"] = json!(64), "vcpu_count");
        rejects(
            |c| c["machine-config"]["mem_size_mib"] = json!(100_000_000u64),
            "mem_size_mib",
        );
        rejects(
            |c| c["boot-source"]["boot_args"] = json!("a\nb"),
            "boot_args",
        );
        rejects(
            |c| c["boot-source"]["boot_args"] = json!("x".repeat(5000)),
            "boot_args",
        );
    }

    #[test]
    fn a_config_that_is_not_json_names_the_vm() {
        let error = parse_vm_config("vm9", b"{ not json").unwrap_err();
        assert!(matches!(error, PlanError::NotJson { .. }));
        assert!(error.to_string().contains("vm9"));
    }

    #[test]
    fn vm_names_are_plain_path_components() {
        for good in ["temps-sandbox-wl_app.1", "a", "A-b_c.d"] {
            assert!(validate_vm_name(good).is_ok(), "{good}");
        }
        for bad in ["", "../x", "a/b", ".hidden", "-lead", "sp ace", "a;b", "nul\0"] {
            assert!(validate_vm_name(bad).is_err(), "{bad:?}");
        }
        assert!(validate_vm_name(&"x".repeat(200)).is_err());
    }

    #[test]
    fn the_jailer_runs_the_vmm_as_the_vms_own_identity() {
        let args: Vec<String> = jailer_args(&config(), "vm1-abc", 52001, 52001)
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "--id",
                "vm1-abc",
                "--exec-file",
                "/data/firecracker/bin/firecracker",
                "--uid",
                "52001",
                "--gid",
                "52001",
                "--chroot-base-dir",
                "/data/firecracker/jail",
                "--",
                "--no-api",
                "--config-file",
                "vm.json",
            ]
        );
    }

    #[test]
    fn each_vm_keeps_its_slot_and_the_range_runs_out() {
        let mut table = SlotTable::default();
        assert_eq!(table.slot_for("a", 3), Some(0));
        assert_eq!(table.slot_for("b", 3), Some(1));
        assert_eq!(table.slot_for("a", 3), Some(0), "stable");
        assert_eq!(table.slot_for("c", 3), Some(2));
        assert_eq!(table.slot_for("d", 3), None, "no slot left");
        assert!(table.release("b"));
        assert_eq!(table.slot_for("d", 3), Some(1), "a freed slot is reused");
        assert!(!table.release("never-had-one"));
        assert_eq!(table.len(), 3);
    }

    #[test]
    fn identities_stay_inside_the_configured_range() {
        let config = config();
        assert_eq!(identity_for_slot(&config, 0), Some((52000, 52000)));
        assert_eq!(identity_for_slot(&config, 2), Some((52002, 52002)));
        assert_eq!(identity_for_slot(&config, 3), None);
    }

    #[test]
    fn containment_refuses_parent_components_and_relative_paths() {
        let root = Path::new("/data/firecracker");
        assert!(is_within(root, Path::new("/data/firecracker/vms/a")));
        assert!(!is_within(root, Path::new("/data/firecracker/../etc")));
        assert!(!is_within(root, Path::new("/data/other")));
        assert!(!is_within(root, Path::new("vms/a")));
    }
}
