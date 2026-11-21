// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use crate::test_support::docker_or_skip;
use futures::StreamExt;

// An Alpine-shaped identity: the same lines the daemon-checked table in
// `resolve_image_user` was derived from.
const PASSWD: &str = "\
root:x:0:0:root:/root:/bin/sh
bin:x:1:1:bin:/bin:/sbin/nologin
daemon:x:2:2:daemon:/sbin:/sbin/nologin
nobody:x:65534:65534:nobody:/:/sbin/nologin
nextjs:x:1001:1001::/home/nextjs:/bin/sh
";
const GROUP: &str = "\
root:x:0:root
bin:x:1:root,bin,daemon
daemon:x:2:root,bin,daemon
adm:x:4:root,adm,daemon
tty:x:5:
nobody:x:65534:
nodejs:x:1001:
";

fn resolve(spec: &str) -> Result<ResolvedUser, ImageUserError> {
    resolve_image_user(spec, Some(PASSWD), Some(GROUP))
}

fn user(uid: u32, gid: u32, groups: &[u32], home: &str) -> ResolvedUser {
    ResolvedUser {
        uid,
        gid,
        groups: groups.to_vec(),
        home: home.to_string(),
    }
}

#[test]
fn a_named_user_takes_its_primary_group_membership_groups_and_home() {
    assert_eq!(
        resolve("daemon").unwrap(),
        user(2, 2, &[1, 2, 4], "/sbin"),
        "primary group plus every group that lists the user"
    );
    assert_eq!(
        resolve("nextjs").unwrap(),
        user(1001, 1001, &[1001], "/home/nextjs")
    );
}

#[test]
fn a_uid_that_passwd_knows_resolves_like_its_name() {
    assert_eq!(resolve("2").unwrap(), resolve("daemon").unwrap());
    assert_eq!(resolve("1001").unwrap(), resolve("nextjs").unwrap());
}

#[test]
fn a_uid_passwd_does_not_know_runs_as_group_zero_with_home_slash() {
    assert_eq!(resolve("1234").unwrap(), user(1234, 0, &[0], "/"));
}

#[test]
fn an_explicit_group_is_used_alone() {
    assert_eq!(
        resolve("daemon:daemon").unwrap(),
        user(2, 2, &[2], "/sbin"),
        "an explicit group drops the membership groups"
    );
    assert_eq!(resolve("daemon:root").unwrap(), user(2, 0, &[0], "/sbin"));
    assert_eq!(resolve("daemon:5").unwrap(), user(2, 5, &[5], "/sbin"));
    assert_eq!(resolve("nextjs:nodejs").unwrap().gid, 1001);
}

#[test]
fn a_fully_numeric_pair_needs_no_entry_in_either_file() {
    assert_eq!(
        resolve("1234:5678").unwrap(),
        user(1234, 5678, &[5678], "/")
    );
    assert_eq!(
        resolve_image_user("1234:5678", None, None).unwrap(),
        user(1234, 5678, &[5678], "/")
    );
}

#[test]
fn root_resolves_without_a_passwd_and_keeps_its_home_with_one() {
    assert_eq!(
        resolve_image_user("root", None, None).unwrap(),
        user(0, 0, &[0], "/")
    );
    assert_eq!(
        resolve("root").unwrap(),
        user(0, 0, &[0, 1, 2, 4], "/root"),
        "root is a member of bin, daemon and adm in this image"
    );
}

#[test]
fn a_name_wins_over_a_number_and_surrounding_space_is_ignored() {
    let passwd = "\
a:x:7:7::/a:/bin/sh
7:x:8:8::/seven:/bin/sh
";
    assert_eq!(
        resolve_image_user("7", Some(passwd), None).unwrap().uid,
        8,
        "a user literally named 7 is found by name first"
    );
    assert_eq!(resolve("  nextjs ").unwrap().uid, 1001);
}

#[test]
fn an_unknown_user_names_the_spec_and_whether_the_image_has_a_passwd() {
    let with_file = resolve("ghost").unwrap_err();
    assert_eq!(
        with_file,
        ImageUserError::UserNotFound {
            spec: "ghost".into(),
            user: "ghost".into(),
            passwd: "not listed",
        }
    );
    let message = with_file.to_string();
    assert!(message.contains("'ghost'"), "{message}");
    assert!(message.contains("/etc/passwd"), "{message}");

    let without_file = resolve_image_user("ghost", None, None).unwrap_err();
    assert!(
        without_file
            .to_string()
            .contains("the image has no such file"),
        "{without_file}"
    );
}

#[test]
fn an_unknown_group_name_is_refused_but_an_unknown_gid_is_not() {
    let error = resolve("daemon:staff").unwrap_err();
    assert_eq!(
        error,
        ImageUserError::GroupNotFound {
            spec: "daemon:staff".into(),
            group: "staff".into(),
            group_file: "not listed",
        }
    );
    assert!(error.to_string().contains("/etc/group"), "{error}");
    assert_eq!(resolve("daemon:4242").unwrap().gid, 4242);
}

#[test]
fn malformed_specs_say_what_is_wrong() {
    for (spec, fragment) in [
        (":5", "user part is empty"),
        ("", "user part is empty"),
        ("daemon:", "group part"),
        ("a:b:c", "more than one ':'"),
    ] {
        let error = resolve(spec).unwrap_err();
        assert!(
            matches!(error, ImageUserError::Malformed { .. }),
            "{spec}: {error}"
        );
        assert!(error.to_string().contains(fragment), "{spec}: {error}");
    }
}

#[test]
fn identity_files_tolerate_comments_blank_and_broken_lines() {
    let passwd = "\
# a comment

broken-line-without-fields
nobody:x:notanumber:1:::
app:x:500:500::/srv/app:/bin/sh
";
    let group = "\
# comment
nogid:x:abc:
apps:x:500:app
";
    assert_eq!(
        resolve_image_user("app", Some(passwd), Some(group)).unwrap(),
        user(500, 500, &[500], "/srv/app")
    );
    assert!(resolve_image_user("nobody", Some(passwd), Some(group)).is_err());
}

#[test]
fn an_empty_home_falls_back_to_the_root_directory() {
    let passwd = "svc:x:900:900:::/bin/false\n";
    assert_eq!(
        resolve_image_user("svc", Some(passwd), None).unwrap().home,
        "/"
    );
}

fn tar_with(build: impl FnOnce(&mut tar::Builder<Vec<u8>>)) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    build(&mut builder);
    builder.into_inner().unwrap()
}

fn regular(name: &str, contents: &[u8]) -> Vec<u8> {
    tar_with(|builder| {
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, name, contents).unwrap();
    })
}

#[test]
fn a_regular_file_is_read_back_whole() {
    let tar = regular("passwd", b"root:x:0:0::/root:/bin/sh\n");
    assert_eq!(
        first_archive_file(&tar, "img", "/etc/passwd").unwrap(),
        Some(ArchiveFile::Regular(
            b"root:x:0:0::/root:/bin/sh\n".to_vec()
        ))
    );
}

#[test]
fn a_symlink_reports_its_target_instead_of_content() {
    let tar = tar_with(|builder| {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        builder
            .append_link(&mut header, "passwd", "../usr/etc/passwd")
            .unwrap();
    });
    assert_eq!(
        first_archive_file(&tar, "img", "/etc/passwd").unwrap(),
        Some(ArchiveFile::Symlink("../usr/etc/passwd".to_string()))
    );
}

#[test]
fn an_empty_archive_is_no_file_and_a_directory_is_refused() {
    assert_eq!(
        first_archive_file(&tar_with(|_| {}), "img", "/etc/passwd").unwrap(),
        None
    );
    let dir = tar_with(|builder| {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Directory);
        header.set_size(0);
        header.set_mode(0o755);
        header.set_cksum();
        builder
            .append_data(&mut header, "passwd/", std::io::empty())
            .unwrap();
    });
    let error = first_archive_file(&dir, "img", "/etc/passwd").unwrap_err();
    assert!(
        matches!(error, ImageFileError::NotReadable { .. }),
        "{error}"
    );
    assert!(error.to_string().contains("/etc/passwd"), "{error}");
    assert!(error.to_string().contains("img"), "{error}");
}

#[test]
fn a_file_over_the_cap_is_refused_before_it_is_read() {
    let big = vec![b'x'; MAX_IDENTITY_FILE_BYTES + 1];
    let error = first_archive_file(&regular("passwd", &big), "img", "/etc/passwd").unwrap_err();
    assert_eq!(
        error,
        ImageFileError::TooLarge {
            image: "img".into(),
            path: "/etc/passwd".into(),
            limit: MAX_IDENTITY_FILE_BYTES,
        }
    );
}

#[test]
fn garbage_is_not_mistaken_for_an_archive_entry() {
    let result = first_archive_file(&[0xff; 1024], "img", "/etc/passwd");
    assert!(result.is_err(), "{result:?}");
}

#[test]
fn symlink_targets_stay_inside_the_image() {
    assert_eq!(
        resolve_link_target("/etc/passwd", "/usr/etc/passwd"),
        "/usr/etc/passwd"
    );
    assert_eq!(
        resolve_link_target("/etc/passwd", "../usr/etc/passwd"),
        "/usr/etc/passwd"
    );
    assert_eq!(
        resolve_link_target("/etc/passwd", "passwd.real"),
        "/etc/passwd.real"
    );
    assert_eq!(
        resolve_link_target("/etc/passwd", "../../../../etc/shadow"),
        "/etc/shadow",
        "`..` stops at the image root"
    );
    assert_eq!(resolve_link_target("/etc/passwd", "./a//b/."), "/etc/a/b");
}

/// Skip when no Docker daemon is reachable or the tiny image cannot be
/// pulled: the pure tests above carry the logic, this one proves the
/// archive API round trip against a real daemon.
async fn docker_with_image(test: &str, image: &str) -> Option<Docker> {
    let docker = docker_or_skip(test).await?;
    if docker.inspect_image(image).await.is_err() {
        let mut pull = docker.create_image(
            Some(bollard::query_parameters::CreateImageOptions {
                from_image: Some(image.to_string()),
                ..Default::default()
            }),
            None,
            None,
        );
        while let Some(item) = pull.next().await {
            item.ok()?;
        }
    }
    Some(docker)
}

#[tokio::test]
async fn a_real_image_resolves_like_docker_does() {
    let Some(docker) = docker_with_image(module_path!(), "alpine:3").await else {
        println!("Docker or the alpine image is unavailable, skipping");
        return;
    };
    let read = read_image_identity(&docker, "alpine:3").await;
    assert_eq!(read.leftover, None);
    let identity = read.identity.unwrap();
    let passwd = identity.passwd.as_deref();
    let group = identity.group.as_deref();
    // The same answers `docker run --user <spec> alpine:3 id` gives.
    let daemon = resolve_image_user("daemon", passwd, group).unwrap();
    assert_eq!((daemon.uid, daemon.gid), (2, 2));
    assert_eq!(daemon.groups, vec![1, 2, 4]);
    assert_eq!(daemon.home, "/sbin");
    assert_eq!(
        resolve_image_user("nobody", passwd, group).unwrap().uid,
        65534
    );
    assert_eq!(
        resolve_image_user("1234", passwd, group).unwrap().gid,
        0,
        "an unknown uid runs as group 0"
    );
    assert!(resolve_image_user("nextjs", passwd, group).is_err());
}

/// Build a throwaway image from `dockerfile` with the docker CLI; `None`
/// when the CLI or daemon cannot do it (the test then skips).
fn build_image(tag: &str, dockerfile: &str) -> Option<()> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new("docker")
        .args(["build", "-q", "-t", tag, "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child.stdin.take()?.write_all(dockerfile.as_bytes()).ok()?;
    child.wait().ok()?.success().then_some(())
}

#[tokio::test]
async fn a_named_user_behind_a_symlinked_passwd_resolves() {
    let tag = format!("temps-image-user-test:{}", std::process::id());
    let dockerfile = "FROM alpine:3\n\
         RUN addgroup -g 1001 nodejs && adduser -D -u 1001 -G nodejs nextjs\n\
         RUN mkdir -p /usr/etc && mv /etc/passwd /usr/etc/passwd \
             && ln -s ../usr/etc/passwd /etc/passwd\n\
         USER nextjs\n";
    let Some(docker) = docker_with_image(module_path!(), "alpine:3").await else {
        println!("Docker or the alpine image is unavailable, skipping");
        return;
    };
    if build_image(&tag, dockerfile).is_none() {
        println!("the image build is unavailable, skipping");
        return;
    }
    let identity = read_image_identity(&docker, &tag).await.identity;
    let _ = std::process::Command::new("docker")
        .args(["rmi", "-f", &tag])
        .output();
    let identity = identity.unwrap();
    assert!(
        identity
            .passwd
            .as_deref()
            .is_some_and(|p| p.contains("nextjs")),
        "the passwd behind the symlink was read: {identity:?}"
    );
    let resolved = resolve_image_user(
        "nextjs",
        identity.passwd.as_deref(),
        identity.group.as_deref(),
    )
    .unwrap();
    assert_eq!((resolved.uid, resolved.gid), (1001, 1001));
    assert_eq!(resolved.home, "/home/nextjs");
    let by_pair = resolve_image_user(
        "nextjs:nodejs",
        identity.passwd.as_deref(),
        identity.group.as_deref(),
    )
    .unwrap();
    assert_eq!((by_pair.uid, by_pair.gid), (1001, 1001));
}

#[tokio::test]
async fn an_image_without_user_databases_resolves_numeric_users_only() {
    let tag = format!("temps-image-user-scratch:{}", std::process::id());
    let Some(docker) =
        docker_or_skip("an_image_without_user_databases_resolves_numeric_users_only").await
    else {
        return;
    };
    if build_image(&tag, "FROM scratch\nLABEL temps.test=1\n").is_none() {
        println!("the image build is unavailable, skipping");
        return;
    }
    let identity = read_image_identity(&docker, &tag).await.identity;
    let _ = std::process::Command::new("docker")
        .args(["rmi", "-f", &tag])
        .output();
    let identity = identity.unwrap();
    assert_eq!(identity, ImageIdentity::default());
    let resolved = resolve_image_user("1000:1000", None, None).unwrap();
    assert_eq!((resolved.uid, resolved.gid), (1000, 1000));
    let error = resolve_image_user("app", None, None).unwrap_err();
    assert!(
        error.to_string().contains("the image has no such file"),
        "{error}"
    );
}

/// The names of the anonymous volumes on the daemon. A listing that
/// fails fails the test: an empty set from a failed command would let the
/// leak check pass without checking anything.
fn anonymous_volumes() -> std::collections::BTreeSet<String> {
    let output = std::process::Command::new("docker")
        .args(["volume", "ls", "-q", "--filter", "dangling=true"])
        .output()
        .expect("run `docker volume ls`");
    assert!(
        output.status.success(),
        "`docker volume ls` failed ({}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

#[tokio::test]
async fn reading_an_image_with_volumes_leaves_no_volume_behind() {
    let tag = format!("temps-image-user-volume:{}", std::process::id());
    let Some(docker) = docker_with_image(module_path!(), "alpine:3").await else {
        println!("Docker or the alpine image is unavailable, skipping");
        return;
    };
    if build_image(&tag, "FROM alpine:3\nVOLUME /data /cache\n").is_none() {
        println!("the image build is unavailable, skipping");
        return;
    }
    let before = anonymous_volumes();
    let identity = read_image_identity(&docker, &tag).await.identity;
    let after = anonymous_volumes();
    let _ = std::process::Command::new("docker")
        .args(["rmi", "-f", &tag])
        .output();
    identity.unwrap();
    let leaked: Vec<_> = after.difference(&before).collect();
    assert!(
        leaked.is_empty(),
        "reading an image's identity left anonymous volumes behind: {leaked:?}"
    );
}

#[tokio::test]
async fn reading_the_identity_of_a_missing_image_names_the_image() {
    let Some(docker) =
        docker_or_skip("reading_the_identity_of_a_missing_image_names_the_image").await
    else {
        return;
    };
    let read = read_image_identity(&docker, "temps-no-such-image:never").await;
    assert_eq!(read.leftover, None, "no container was created");
    let error = read.identity.unwrap_err();
    assert!(
        error.to_string().contains("temps-no-such-image:never"),
        "{error}"
    );
}

fn identity(passwd: Option<&str>, group: Option<&str>) -> ImageIdentity {
    ImageIdentity {
        passwd: passwd.map(str::to_string),
        group: group.map(str::to_string),
    }
}

#[test]
fn no_user_root_zero_and_uid_zero_with_any_group_are_root() {
    let none = ImageIdentity::default();
    for spec in [
        "",
        "   ",
        "root",
        "0",
        "0:0",
        "0:5",
        "0:staff",
        "root:root",
        "root:staff",
    ] {
        assert_eq!(
            classify_image_user(spec, &none),
            ImageUserVerdict::Root,
            "USER '{spec}' runs as uid 0"
        );
    }
}

#[test]
fn a_name_passwd_maps_to_uid_zero_is_root() {
    let passwd = "toor:x:0:0::/root:/bin/sh\napp:x:10001:10001::/app:/bin/sh\n";
    assert!(classify_image_user("toor", &identity(Some(passwd), None)).is_root());
    assert_eq!(
        classify_image_user("app", &identity(Some(passwd), None)),
        ImageUserVerdict::NonRoot { uid: 10001 }
    );
}

#[test]
fn named_and_numeric_non_root_users_resolve_against_the_image() {
    let image = identity(Some(PASSWD), Some(GROUP));
    assert_eq!(
        classify_image_user("nextjs", &image),
        ImageUserVerdict::NonRoot { uid: 1001 }
    );
    assert_eq!(
        classify_image_user("nextjs:nodejs", &image),
        ImageUserVerdict::NonRoot { uid: 1001 }
    );
    // Numbers need neither file.
    let none = ImageIdentity::default();
    assert_eq!(
        classify_image_user("10001", &none),
        ImageUserVerdict::NonRoot { uid: 10001 }
    );
    assert_eq!(
        classify_image_user("10001:10001", &none),
        ImageUserVerdict::NonRoot { uid: 10001 }
    );
}

#[test]
fn an_unresolvable_named_user_is_unresolved_not_root() {
    let verdict = classify_image_user("ghost", &identity(Some(PASSWD), Some(GROUP)));
    assert!(
        matches!(
            verdict,
            ImageUserVerdict::Unresolved(ImageUserError::UserNotFound { .. })
        ),
        "{verdict:?}"
    );
    assert!(!verdict.is_root());
    // No passwd at all (a scratch image) is the same answer.
    assert!(matches!(
        classify_image_user("app", &ImageIdentity::default()),
        ImageUserVerdict::Unresolved(_)
    ));
    // A known user with a group the image lacks is unresolved too: the
    // runtime would refuse it.
    assert!(matches!(
        classify_image_user("nextjs:staff", &identity(Some(PASSWD), Some(GROUP))),
        ImageUserVerdict::Unresolved(ImageUserError::GroupNotFound { .. })
    ));
    assert!(matches!(
        classify_image_user(":5", &ImageIdentity::default()),
        ImageUserVerdict::Unresolved(ImageUserError::Malformed { .. })
    ));
}

#[test]
fn only_names_make_the_classifier_read_identity_files() {
    let none = IdentityFilesNeeded::default();
    for spec in [
        "",
        "root",
        "0",
        "1000",
        "1000:1000",
        "0:0",
        "0:staff",
        "root:staff",
    ] {
        assert_eq!(IdentityFilesNeeded::to_classify(spec), none, "{spec}");
    }
    assert_eq!(
        IdentityFilesNeeded::to_classify("app"),
        IdentityFilesNeeded {
            passwd: true,
            group: false
        }
    );
    assert_eq!(
        IdentityFilesNeeded::to_classify("1000:app"),
        IdentityFilesNeeded {
            passwd: false,
            group: true
        }
    );
    assert_eq!(
        IdentityFilesNeeded::to_classify("app:app"),
        IdentityFilesNeeded::ALL
    );
}

/// An in-memory image: path → archive, and every path asked for.
struct FakeImage {
    files: std::collections::HashMap<&'static str, Vec<u8>>,
    asked: std::sync::Mutex<Vec<String>>,
}

impl FakeImage {
    fn new(files: Vec<(&'static str, Vec<u8>)>) -> Self {
        Self {
            files: files.into_iter().collect(),
            asked: std::sync::Mutex::new(Vec::new()),
        }
    }

    async fn read(&self, needed: IdentityFilesNeeded) -> Result<ImageIdentity, ImageFileError> {
        read_identity_files("my-app:1", needed, |path| async move {
            self.asked.lock().unwrap().push(path.clone());
            Ok(match self.files.get(path.as_str()) {
                Some(archive) => FetchedPath::Archive(archive.clone()),
                None => FetchedPath::Missing,
            })
        })
        .await
    }
}

fn symlink(name: &str, target: &str) -> Vec<u8> {
    tar_with(|builder| {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        builder.append_link(&mut header, name, target).unwrap();
    })
}

#[tokio::test]
async fn identity_files_are_read_through_any_fetcher_following_symlinks() {
    let image = FakeImage::new(vec![
        ("/etc/passwd", symlink("passwd", "../usr/etc/passwd")),
        ("/usr/etc/passwd", regular("passwd", PASSWD.as_bytes())),
        ("/etc/group", regular("group", GROUP.as_bytes())),
    ]);
    let read = image.read(IdentityFilesNeeded::ALL).await.unwrap();
    assert_eq!(read.passwd.as_deref(), Some(PASSWD));
    assert_eq!(read.group.as_deref(), Some(GROUP));
    assert_eq!(
        classify_image_user("nextjs", &read),
        ImageUserVerdict::NonRoot { uid: 1001 }
    );
}

#[tokio::test]
async fn a_fetcher_is_asked_only_for_the_files_needed() {
    let image = FakeImage::new(vec![("/etc/passwd", regular("passwd", PASSWD.as_bytes()))]);
    let read = image
        .read(IdentityFilesNeeded::to_classify("nextjs"))
        .await
        .unwrap();
    assert_eq!(read.group, None);
    assert_eq!(
        *image.asked.lock().unwrap(),
        vec!["/etc/passwd".to_string()]
    );

    let image = FakeImage::new(vec![]);
    let read = image
        .read(IdentityFilesNeeded::to_classify(""))
        .await
        .unwrap();
    assert_eq!(read, ImageIdentity::default());
    assert!(
        image.asked.lock().unwrap().is_empty(),
        "no USER reads nothing"
    );
}

#[tokio::test]
async fn a_missing_file_reads_as_none_and_a_symlink_loop_is_refused() {
    let image = FakeImage::new(vec![]);
    assert_eq!(
        image.read(IdentityFilesNeeded::ALL).await.unwrap(),
        ImageIdentity::default()
    );

    let image = FakeImage::new(vec![
        ("/etc/passwd", symlink("passwd", "/etc/passwd2")),
        ("/etc/passwd2", symlink("passwd2", "/etc/passwd")),
    ]);
    let error = image.read(IdentityFilesNeeded::ALL).await.unwrap_err();
    assert_eq!(
        error,
        ImageFileError::SymlinkLoop {
            image: "my-app:1".into(),
            path: "/etc/passwd".into(),
            limit: MAX_SYMLINK_HOPS,
        }
    );
}

#[tokio::test]
async fn an_oversized_archive_from_a_fetcher_is_refused() {
    let image = FakeImage::new(vec![(
        "/etc/passwd",
        vec![0u8; MAX_IDENTITY_ARCHIVE_BYTES + 1],
    )]);
    let error = image.read(IdentityFilesNeeded::ALL).await.unwrap_err();
    assert!(matches!(error, ImageFileError::TooLarge { .. }), "{error}");
}

#[tokio::test]
async fn collecting_a_stream_stops_at_the_cap_and_names_failures() {
    let chunk = vec![0u8; 64 * 1024];
    let chunks = (0..(MAX_IDENTITY_ARCHIVE_BYTES / chunk.len()) + 2)
        .map(|_| Ok::<_, std::io::Error>(chunk.clone()));
    let error = collect_identity_archive(futures::stream::iter(chunks), "my-app:1", "/etc/passwd")
        .await
        .unwrap_err();
    assert!(matches!(error, ImageFileError::TooLarge { .. }), "{error}");

    let failing = futures::stream::iter(vec![
        Ok(b"abc".to_vec()),
        Err(std::io::Error::other("connection reset")),
    ]);
    let error = collect_identity_archive(failing, "my-app:1", "/etc/passwd")
        .await
        .unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("my-app:1") && message.contains("/etc/passwd"),
        "{message}"
    );
    assert!(message.contains("connection reset"), "{message}");

    let whole = collect_identity_archive(
        futures::stream::iter(vec![
            Ok::<_, std::io::Error>(b"ab".to_vec()),
            Ok(b"cd".to_vec()),
        ]),
        "my-app:1",
        "/etc/passwd",
    )
    .await
    .unwrap();
    assert_eq!(whole, b"abcd");
}
