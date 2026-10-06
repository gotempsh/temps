// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Generated-preset regressions with disposable sources and an isolated BuildKit cache.
//! These exercise images directly; they do not claim console or published-artifact qualification.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use temps_presets::{AutopackPreset, DockerfileConfig, Preset, Vite};

fn docker(args: &[&str]) -> String {
    let output = Command::new("docker")
        .args(args)
        .output()
        .expect("docker command");
    assert!(
        output.status.success(),
        "docker {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

struct Resources {
    builder: String,
    images: Vec<String>,
    containers: Vec<String>,
}

impl Resources {
    fn new() -> Option<Self> {
        if !Command::new("docker")
            .arg("info")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
        {
            eprintln!("Docker unavailable; skipping generated-preset image scenarios");
            return None;
        }
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let builder = format!("temps-preset-test-{}-{id}", std::process::id());
        let resources = Self {
            builder,
            images: vec![],
            containers: vec![],
        };
        docker(&[
            "buildx",
            "create",
            "--name",
            &resources.builder,
            "--driver",
            "docker-container",
        ]);
        Some(resources)
    }

    fn build(&mut self, root: &Path, dockerfile: &str, label: &str) -> String {
        let tag = format!("{}-{label}:test", self.builder);
        if !self.images.contains(&tag) {
            self.images.push(tag.clone());
        }
        std::fs::write(root.join("Dockerfile"), dockerfile).unwrap();
        // No default/shared builder cache is read, written or pruned.
        docker(&[
            "buildx",
            "build",
            "--builder",
            &self.builder,
            "--load",
            "--progress",
            "plain",
            "-t",
            &tag,
            root.to_str().unwrap(),
        ]);
        tag
    }

    fn start(&mut self, image: &str, port: u16) -> String {
        let name = format!("{}-run-{}", self.builder, self.containers.len());
        self.containers.push(name.clone());
        docker(&[
            "run",
            "-d",
            "--name",
            &name,
            "--user",
            "10001:10001",
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges:true",
            "-e",
            &format!("PORT={port}"),
            "-e",
            &format!("SECRET_KEY_BASE={}", name.repeat(3)),
            "-p",
            &format!("127.0.0.1::{port}"),
            image,
        ]);
        name
    }

    fn url(&self, name: &str, port: u16) -> String {
        let published = docker(&["port", name, &format!("{port}/tcp")]);
        format!("http://{published}")
    }
}

impl Drop for Resources {
    fn drop(&mut self) {
        for name in &self.containers {
            if std::thread::panicking() {
                if let Ok(output) = Command::new("docker")
                    .args(["logs", "--tail", "30", name])
                    .output()
                {
                    eprintln!(
                        "{name}: {}{}",
                        String::from_utf8_lossy(&output.stdout),
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
            }
            let _ = Command::new("docker").args(["rm", "-f", name]).output();
        }
        for image in &self.images {
            let _ = Command::new("docker").args(["image", "rm", image]).output();
        }
        let _ = Command::new("docker")
            .args(["buildx", "rm", &self.builder])
            .output();
    }
}

fn fixture(root: &Path, files: &[(&str, &str)]) {
    for (path, contents) in files {
        let target = root.join(path);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(target, contents).unwrap();
    }
}

fn response(url: &str, expected: &str, status: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let output = Command::new("curl")
            .args([
                "--silent",
                "--show-error",
                "--max-time",
                "2",
                "-w",
                "\n%{http_code}",
                url,
            ])
            .output()
            .unwrap();
        let body = String::from_utf8_lossy(&output.stdout);
        if output.status.success() && body.contains(expected) && body.trim_end().ends_with(status) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{url}: expected {expected} / {status}, got {body}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[tokio::test]
async fn nested_pnpm_node_image_retains_sibling_package_at_runtime() {
    let Some(mut resources) = Resources::new() else {
        return;
    };
    let repo = tempfile::tempdir().unwrap();
    fixture(repo.path(), &[
        ("package.json", r#"{"private":true,"packageManager":"pnpm@10.15.1","scripts":{"postinstall":"node -e \"require('fs').writeFileSync('root-install-ok','ok')\"","start":"node wrong.js","build":"node wrong.js"}}"#),
        ("pnpm-workspace.yaml", "packages:\n  - apps/*\n  - packages/*\n"),
        ("apps/api/package.json", r#"{"name":"@fixture/api","scripts":{"start":"node server.js"},"dependencies":{"@fixture/shared":"workspace:*"}}"#),
        ("apps/api/server.js", "const shared = require('@fixture/shared'); require('http').createServer((_, res) => res.end(shared)).listen(process.env.PORT, '0.0.0.0');\n"),
        ("packages/shared/package.json", r#"{"name":"@fixture/shared","version":"1.0.0","main":"index.js"}"#),
        ("packages/shared/index.js", "module.exports = 'workspace-runtime-ok';\n"),
        ("pnpm-lock.yaml", "lockfileVersion: '9.0'\nsettings:\n  autoInstallPeers: true\n  excludeLinksFromLockfile: false\nimporters:\n  .: {}\n  apps/api:\n    dependencies:\n      '@fixture/shared':\n        specifier: workspace:*\n        version: link:../../packages/shared\n  packages/shared: {}\n"),
    ]);
    let app = repo.path().join("apps/api");
    let mut config = DockerfileConfig::new(repo.path(), &app, "fixture");
    config.use_buildkit = true;
    let dockerfile = AutopackPreset.dockerfile(config).await.content;
    let image = resources.build(repo.path(), &dockerfile, "node");
    let name = resources.start(&image, 3417);
    response(&resources.url(&name, 3417), "workspace-runtime-ok", "200");
    docker(&["exec", &name, "test", "-f", "/app/root-install-ok"]);
    let manifest = app.join("package.json");
    let contents = std::fs::read_to_string(&manifest)
        .unwrap()
        .replace("workspace:*", "workspace:^");
    std::fs::write(manifest, contents).unwrap();
    let output = Command::new("docker")
        .args([
            "buildx",
            "build",
            "--builder",
            &resources.builder,
            "--progress",
            "plain",
            repo.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "stale workspace lockfile was accepted"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("ERR_PNPM_OUTDATED_LOCKFILE"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn php_image_runs_restarts_and_has_no_disabled_admin_probe() {
    let Some(mut resources) = Resources::new() else {
        return;
    };
    let repo = tempfile::tempdir().unwrap();
    fixture(repo.path(), &[
        ("composer.json", r#"{"require":{"php":"8.4.*","laravel/framework":"^12.0"}}"#),
        ("public/index.php", "<?php require __DIR__.'/../vendor/autoload.php'; $app = require_once __DIR__.'/../bootstrap/app.php'; $app->handleRequest(Illuminate\\Http\\Request::capture());\n"),
        ("bootstrap/app.php", "<?php return Illuminate\\Foundation\\Application::configure(basePath: dirname(__DIR__))->withRouting(api: __DIR__.'/../routes/api.php', apiPrefix: '')->withMiddleware(function (Illuminate\\Foundation\\Configuration\\Middleware $middleware) {})->withExceptions(function (Illuminate\\Foundation\\Configuration\\Exceptions $exceptions) {})->create();\n"),
        ("routes/api.php", "<?php Illuminate\\Support\\Facades\\Route::get('/', fn () => response()->json(['framework' => 'laravel', 'fixture' => 'php-runtime-ok'])); Illuminate\\Support\\Facades\\Route::get('/unready', fn () => response('intentional-unready', 503));\n"),
        ("bootstrap/cache/.keep", ""),
        ("storage/framework/cache/data/.keep", ""),
        ("storage/framework/views/.keep", ""),
        ("storage/logs/.keep", ""),
    ]);
    let mut config = DockerfileConfig::new(repo.path(), repo.path(), "fixture");
    config.use_buildkit = true;
    let dockerfile = AutopackPreset.dockerfile(config).await.content;
    let image = resources.build(repo.path(), &dockerfile, "php");
    for port in [3000, 3417] {
        let name = resources.start(&image, port);
        let url = resources.url(&name, port);
        response(&url, "php-runtime-ok", "200");
        response(&format!("{url}/unready"), "intentional-unready", "503");
        assert_eq!(
            docker(&[
                "inspect",
                "--format",
                "{{json .Config.Healthcheck.Test}}",
                &name
            ]),
            r#"["NONE"]"#
        );
        for _ in 0..2 {
            docker(&["restart", &name]);
            response(&resources.url(&name, port), "php-runtime-ok", "200");
        }
    }
}

#[tokio::test]
async fn ruby_locked_cold_and_warm_images_preserve_gems_and_interpreter() {
    let Some(mut resources) = Resources::new() else {
        return;
    };
    for (version, bundler) in [("3.3", "2.5.23"), ("3.4", "2.6.9")] {
        let repo = tempfile::tempdir().unwrap();
        let pin = format!("ruby-{version}");
        let lock = format!("GEM\n  remote: https://rubygems.org/\n  specs:\n    rack (3.1.8)\n\nPLATFORMS\n  ruby\n\nDEPENDENCIES\n  rack (= 3.1.8)\n\nBUNDLED WITH\n   {bundler}\n");
        fixture(repo.path(), &[
            ("Gemfile", "source 'https://rubygems.org'\ngem 'rack', '3.1.8'\n"),
            ("Gemfile.lock", &lock),
            (".ruby-version", &pin),
            ("Procfile", "web: bundle exec ruby server.rb\n"),
            ("server.rb", "require 'rack'\nrequire 'socket'\nserver = TCPServer.new('0.0.0.0', ENV.fetch('PORT'))\nloop do\n client = server.accept\n client.gets\n body = 'ruby-runtime-ok-' + RUBY_VERSION + '-rack-' + Rack.release\n client.write(\"HTTP/1.1 200 OK\\r\\nContent-Length: #{body.bytesize}\\r\\nConnection: close\\r\\n\\r\\n#{body}\")\n client.close\nend\n"),
        ]);
        let cache_settings = vec![
            "AUTOPACK_CACHE_SCOPE=app".to_string(),
            format!("AUTOPACK_CACHE_KEY={}-{version}", resources.builder),
        ];
        let mut config = DockerfileConfig::new(repo.path(), repo.path(), "fixture");
        config.use_buildkit = true;
        config.build_vars = Some(&cache_settings);
        let dockerfile = AutopackPreset.dockerfile(config).await.content;
        for warm in [false, true] {
            if warm {
                let gemfile = repo.path().join("Gemfile");
                let original = std::fs::read_to_string(&gemfile).unwrap();
                std::fs::write(
                    gemfile,
                    format!("{original}\n# Reinstall using the warm test-owned cache\n"),
                )
                .unwrap();
            }
            let image = resources.build(repo.path(), &dockerfile, &format!("ruby-{version}"));
            let name = resources.start(&image, 3417);
            response(
                &resources.url(&name, 3417),
                &format!("ruby-runtime-ok-{version}"),
                "200",
            );
        }
    }
}

#[tokio::test]
async fn nested_vite_image_builds_real_bundle_importing_sibling() {
    let Some(mut resources) = Resources::new() else {
        return;
    };
    let repo = tempfile::tempdir().unwrap();
    fixture(repo.path(), &[
        ("package.json", r#"{"private":true,"packageManager":"pnpm@10.15.1"}"#),
        ("pnpm-workspace.yaml", "packages:\n  - apps/*\n  - packages/*\n"),
        ("apps/web/package.json", r#"{"name":"@fixture/web","type":"module","scripts":{"build":"vite build"},"dependencies":{"@fixture/shared":"workspace:*"},"devDependencies":{"vite":"6.1.0"}}"#),
        ("apps/web/index.html", "<div id=app></div><script type=module src=/main.js></script>"),
        ("apps/web/main.js", "import { message } from '@fixture/shared'; document.querySelector('#app').textContent = message;"),
        ("packages/shared/package.json", r#"{"name":"@fixture/shared","version":"1.0.0","type":"module","exports":"./index.js"}"#),
        ("packages/shared/index.js", "export const message = 'vite-workspace-bundle-ok';"),
    ]);
    // Resolve a real committed-lockfile equivalent inside a disposable image/container.
    let resolver = resources.build(repo.path(), "FROM node:22\nWORKDIR /app\nRUN corepack enable\nCOPY . .\nRUN pnpm install --lockfile-only\n", "lock-resolver");
    let name = format!("{}-lock-export", resources.builder);
    resources.containers.push(name.clone());
    docker(&["create", "--name", &name, &resolver]);
    docker(&[
        "cp",
        &format!("{name}:/app/pnpm-lock.yaml"),
        repo.path().join("pnpm-lock.yaml").to_str().unwrap(),
    ]);
    let app = repo.path().join("apps/web");
    let dockerfile = Vite
        .dockerfile(DockerfileConfig::new(repo.path(), &app, "fixture"))
        .await
        .content;
    let image = resources.build(repo.path(), &dockerfile, "vite");
    let container = format!("{}-vite-export", resources.builder);
    resources.containers.push(container.clone());
    docker(&["create", "--name", &container, &image]);
    let output = tempfile::tempdir().unwrap();
    docker(&[
        "cp",
        &format!("{container}:/usr/share/nginx/html/."),
        output.path().to_str().unwrap(),
    ]);
    assert!(std::fs::read_dir(output.path().join("assets"))
        .unwrap()
        .flatten()
        .any(
            |entry| entry.path().extension().is_some_and(|ext| ext == "js")
                && std::fs::read_to_string(entry.path())
                    .unwrap()
                    .contains("vite-workspace-bundle-ok")
        ));
    // Optional browser check uses an explicitly supplied local Chromium executable.
    // The nginx container is only a disposable server for the extracted static assets.
    if let Ok(chromium) = std::env::var("TEMPS_TEST_CHROMIUM") {
        let name = format!("{}-vite-browser", resources.builder);
        resources.containers.push(name.clone());
        docker(&["run", "-d", "--name", &name, "-p", "127.0.0.1::80", &image]);
        let url = resources.url(&name, 80);
        response(&url, "<script", "200");
        let profile = tempfile::tempdir().unwrap();
        let browser = Command::new(chromium)
            .args([
                "--headless",
                "--disable-gpu",
                "--dump-dom",
                "--virtual-time-budget=5000",
                &format!("--user-data-dir={}", profile.path().display()),
                &url,
            ])
            .output()
            .unwrap();
        assert!(
            browser.status.success(),
            "{}",
            String::from_utf8_lossy(&browser.stderr)
        );
        assert!(
            String::from_utf8_lossy(&browser.stdout)
                .contains(r#"id="app">vite-workspace-bundle-ok"#),
            "{}",
            String::from_utf8_lossy(&browser.stdout)
        );
    }
}

#[tokio::test]
async fn locked_rails_framework_route_builds_from_cold_and_warm_cache() {
    let Some(mut resources) = Resources::new() else {
        return;
    };
    let repo = tempfile::tempdir().unwrap();
    fixture(repo.path(), &[
        ("Gemfile", "source 'https://rubygems.org'\ngem 'rails', '8.0.5.1'\ngem 'puma', '6.6.1'\n"),
        (".ruby-version", "ruby-3.4\n"),
        ("config/application.rb", "require 'rails'\nrequire 'action_controller/railtie'\nmodule Fixture\n class Application < Rails::Application\n  config.eager_load = true\n  config.hosts.clear\n  config.secret_key_base = ENV.fetch('SECRET_KEY_BASE') { SecureRandom.hex(64) }\n  routes.append { get '/', to: ->(_env) { [200, {'content-type' => 'text/plain'}, ['rails-framework-ok']] } }\n end\nend\n"),
        ("config.ru", "require_relative 'config/application'\nFixture::Application.initialize!\nrun Fixture::Application\n"),
        ("Procfile", "web: bundle exec puma -b tcp://0.0.0.0:$PORT\n"),
    ]);
    let resolver = resources.build(repo.path(), "FROM ruby:3.4-slim\nWORKDIR /app\nCOPY Gemfile .\nRUN gem install bundler -v 2.6.9 --no-document && bundle _2.6.9_ lock\n", "rails-lock");
    let export = format!("{}-rails-lock-export", resources.builder);
    resources.containers.push(export.clone());
    docker(&["create", "--name", &export, &resolver]);
    docker(&[
        "cp",
        &format!("{export}:/app/Gemfile.lock"),
        repo.path().join("Gemfile.lock").to_str().unwrap(),
    ]);
    let mut config = DockerfileConfig::new(repo.path(), repo.path(), "fixture");
    config.use_buildkit = true;
    let dockerfile = AutopackPreset.dockerfile(config).await.content;
    for warm in [false, true] {
        if warm {
            let gemfile = repo.path().join("Gemfile");
            let original = std::fs::read_to_string(&gemfile).unwrap();
            std::fs::write(
                gemfile,
                format!("{original}\n# Reinstall using the warm test-owned cache\n"),
            )
            .unwrap();
        }
        let image = resources.build(repo.path(), &dockerfile, "rails");
        let name = resources.start(&image, 3417);
        response(&resources.url(&name, 3417), "rails-framework-ok", "200");
    }
}
