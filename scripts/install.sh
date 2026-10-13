#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

# Temps installer script - inspired by Bun's installation approach
set -euo pipefail

platform=$(uname -ms)

if [[ ${OS:-} = Windows_NT ]]; then
  echo "Windows is not yet supported. Please use WSL2 or download the binary manually."
  exit 1
fi

# Reset
Color_Off=''

# Regular Colors
Red=''
Green=''
Dim=''
Yellow=''

# Bold
Bold_White=''
Bold_Green=''

if [[ -t 1 ]]; then
    # Reset
    Color_Off='\033[0m'

    # Regular Colors
    Red='\033[0;31m'
    Green='\033[0;32m'
    Dim='\033[0;2m'
    Yellow='\033[0;33m'

    # Bold
    Bold_Green='\033[1;32m'
    Bold_White='\033[1m'
fi

error() {
    echo -e "${Red}error${Color_Off}:" "$@" >&2
    exit 1
}

info() {
    echo -e "${Dim}$@ ${Color_Off}"
}

info_bold() {
    echo -e "${Bold_White}$@ ${Color_Off}"
}

success() {
    echo -e "${Green}$@ ${Color_Off}"
}

warning() {
    echo -e "${Yellow}warning${Color_Off}:" "$@"
}

# Verify a downloaded file against its published `.sha256` sibling asset
# (ADR-020 WS-7 / supplychain-1, supplychain-8). We fail CLOSED: if the
# checksum cannot be fetched or does not match, we refuse to install rather
# than silently running an unverified binary. An explicit, loud opt-out
# (TEMPS_INSTALL_SKIP_CHECKSUM=1) exists for the rare case a release lacks
# the asset, so users are never hard-blocked — but the default is verified.
verify_checksum() {
    local file="$1" url_sha="$2"
    local sha_file="$file.sha256" expected actual

    if ! curl --fail --silent --location --output "$sha_file" "$url_sha" 2>/dev/null; then
        rm -f "$sha_file"
        if [[ "${TEMPS_INSTALL_SKIP_CHECKSUM:-}" = "1" ]]; then
            warning "Checksum not found at \"$url_sha\"; skipping verification (TEMPS_INSTALL_SKIP_CHECKSUM=1)."
            return 0
        fi
        rm -f "$file"
        error "Could not download a checksum from \"$url_sha\" to verify the binary.
Refusing to install an unverified binary. To override (NOT recommended), re-run with
TEMPS_INSTALL_SKIP_CHECKSUM=1."
    fi

    # Accept both '<hash>' and '<hash>  filename' formats: take the first 64-hex token.
    expected=$(grep -oE '[0-9a-fA-F]{64}' "$sha_file" | head -n 1)
    rm -f "$sha_file"
    if [[ -z "$expected" ]]; then
        rm -f "$file"
        error "Checksum file from \"$url_sha\" did not contain a valid SHA-256 digest."
    fi

    if command -v sha256sum >/dev/null 2>&1; then
        actual=$(sha256sum "$file" | awk '{print $1}')
    elif command -v shasum >/dev/null 2>&1; then
        actual=$(shasum -a 256 "$file" | awk '{print $1}')
    else
        rm -f "$file"
        error "Neither 'sha256sum' nor 'shasum' is available to verify the download."
    fi

    if [[ "$(printf '%s' "$expected" | tr 'A-F' 'a-f')" != "$(printf '%s' "$actual" | tr 'A-F' 'a-f')" ]]; then
        rm -f "$file"
        error "Checksum verification FAILED — the download may be corrupted or tampered with.
  expected: $expected
  actual:   $actual
Aborting installation."
    fi

    success "Checksum verified (sha256)."
}

command -v curl >/dev/null ||
    error 'curl is required to install temps'

# GET a GitHub API URL without aborting the script. Sets `api_status` (the
# HTTP status, `000` when the request never got a response) and `api_body`,
# so callers can tell "this does not exist" (404) apart from rate limiting
# or a network failure instead of reporting every failure as "not found".
github_api_get() {
    local response
    response=$(curl --silent --location --write-out $'\n%{http_code}' "$1" 2>/dev/null) || true
    api_status=${response##*$'\n'}
    api_body=${response%$'\n'*}
    [[ $api_status =~ ^[0-9]{3}$ ]] || api_status=000
}

# --- Release selection helpers -------------------------------------------
#
# These mirror the release picker in `temps upgrade`
# (crates/temps-cli/src/commands/upgrade.rs: `normalize_release_tag`,
# `version_sort_key` and `pick_installable_release_for_channel`), so the
# installer and the CLI agree on which release a channel resolves to.

# A release version tag: `vMAJOR.MINOR.PATCH` plus an optional `-prerelease`
# made of dot-separated alphanumeric/dash identifiers. Anything else -- the
# `test-v*` tags cut by the release-workflow smoke test, `latest`, path
# fragments -- is not an installable version. The tag ends up in a download
# URL, so keep this strict.
release_tag_pattern='^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$'

# Print the highest of the tags read from stdin (one per line) by semver
# precedence: core version first, a release outranks its own prereleases,
# then prerelease identifiers left to right -- numeric ones compare
# numerically (rc.10 > rc.2) and rank below alphanumeric ones, and a longer
# identifier list wins a shared prefix. Written in POSIX awk because
# `sort -V` is GNU-only and has no prerelease semantics. Callers filter the
# input through `release_tag_pattern` first.
highest_semver_tag() {
    awk '
    function ident_cmp(a, b,   a_num, b_num) {
        a_num = (a ~ /^[0-9]+$/)
        b_num = (b ~ /^[0-9]+$/)
        if (a_num && b_num) {
            if (a + 0 < b + 0) return -1
            if (a + 0 > b + 0) return 1
            return 0
        }
        if (a_num) return -1
        if (b_num) return 1
        if ((a "") < (b "")) return -1
        if ((a "") > (b "")) return 1
        return 0
    }
    function version_cmp(x, y,   x_pre, y_pre, x_core, y_core, x_ids, y_ids, n, m, i, r) {
        x_pre = ""
        y_pre = ""
        if (index(x, "-")) { x_pre = substr(x, index(x, "-") + 1); x = substr(x, 1, index(x, "-") - 1) }
        if (index(y, "-")) { y_pre = substr(y, index(y, "-") + 1); y = substr(y, 1, index(y, "-") - 1) }
        split(x, x_core, ".")
        split(y, y_core, ".")
        for (i = 1; i <= 3; i++) {
            if (x_core[i] + 0 < y_core[i] + 0) return -1
            if (x_core[i] + 0 > y_core[i] + 0) return 1
        }
        if (x_pre == "" && y_pre == "") return 0
        if (x_pre == "") return 1
        if (y_pre == "") return -1
        n = split(x_pre, x_ids, ".")
        m = split(y_pre, y_ids, ".")
        for (i = 1; i <= n && i <= m; i++) {
            r = ident_cmp(x_ids[i], y_ids[i])
            if (r != 0) return r
        }
        if (n < m) return -1
        if (n > m) return 1
        return 0
    }
    NF {
        version = $0
        sub(/^v/, "", version)
        if (best == "" || version_cmp(version, best_version) > 0) {
            best = $0
            best_version = version
        }
    }
    END { if (best != "") print best }
    '
}

# Print, one per line, the version tags of the releases in GitHub releases
# JSON ($1) that ship an asset named $2 -- the same "can this host actually
# install it?" check `temps upgrade` makes, so an asset-less release (a test
# release, or one whose upload failed) is never picked.
#
# Read from each asset's `browser_download_url`
# (`.../releases/download/<tag>/<asset>`) rather than pairing `tag_name`
# with `assets[].name`: that pairing depends on key order and nesting, which
# only a real JSON parser gets right. The URL carries both facts in one
# string, works on minified and pretty-printed JSON alike, and cannot match
# text inside a release body, where the quotes are escaped. Draft releases
# download from `untagged-*` paths and fail the version pattern.
release_tags_with_asset() {
    local asset_pattern="${2//./\\.}"
    printf '%s\n' "$1" |
        grep -oE '"browser_download_url": *"[^"]*/releases/download/[^"/]+/'"$asset_pattern"'"' |
        sed -E 's#.*/releases/download/([^"/]+)/[^"/]+"$#\1#' |
        grep -E "$release_tag_pattern" || true
}

# Is the numeric core (MAJOR.MINOR.PATCH) of tag $1 lower than that of $2?
# Prerelease suffixes are ignored: only a whole release line counts.
core_version_lt() {
    local a b i
    IFS=. read -r -a a <<< "$(echo "${1#v}" | sed -E 's/[-+].*//')"
    IFS=. read -r -a b <<< "$(echo "${2#v}" | sed -E 's/[-+].*//')"
    for i in 0 1 2; do
        [[ "${a[i]:-0}" =~ ^[0-9]+$ && "${b[i]:-0}" =~ ^[0-9]+$ ]] || return 1
        if (( 10#${a[i]:-0} < 10#${b[i]:-0} )); then return 0; fi
        if (( 10#${a[i]:-0} > 10#${b[i]:-0} )); then return 1; fi
    done
    return 1
}

# While Temps is in beta, the newest stable release can belong to an older
# release line than the betas everyone else runs. Installing it is still what
# the default channel promises, so warn rather than silently switch channels.
warn_if_stable_predates_beta() {
    local stable_tag="$1" beta_tag
    github_api_get "https://api.github.com/repos/gotempsh/temps/releases?per_page=100"
    [[ "$api_status" = "200" ]] || return 0
    # Same selection as `--channel beta`, limited to the first page: this is
    # only an advisory, so it must not cost the install more API calls.
    beta_tag=$(release_tags_with_asset "$api_body" "temps-$target.tar.gz" |
               grep -v -- '-nightly\.' |
               highest_semver_tag || true)
    if [[ -n "$beta_tag" ]] && core_version_lt "$stable_tag" "$beta_tag"; then
        warning "$stable_tag is the newest stable release, but it predates the current beta
line (newest: $beta_tag). Temps is in beta, and most installs track the beta channel.
To install the current beta instead:
    curl -fsSL https://raw.githubusercontent.com/gotempsh/temps/main/scripts/install.sh | bash -s -- --channel beta
"
    fi
}

# Explain a GitHub API failure that is not "no such release".
github_api_error() {
    local what="$1" hint=""
    case "$api_status" in
        000) hint="no response (network or DNS failure)" ;;
        403|429) hint="HTTP $api_status (GitHub API rate limit; wait a few minutes or pin a version)" ;;
        *) hint="HTTP $api_status" ;;
    esac
    error "Could not query GitHub for $what: $hint.
Pin a version to skip the lookup:
    curl -fsSL https://raw.githubusercontent.com/gotempsh/temps/main/scripts/install.sh | bash -s -- <version>

Available versions: https://github.com/gotempsh/temps/releases"
}

# Channel selection. Mirrors `temps upgrade --channel`:
#   stable (default) — track non-prerelease tags only
#   beta             — track the highest version, prerelease or not, EXCLUDING
#                       nightly builds (a `-nightly.` tag never satisfies beta)
#   nightly          — track only automated nightly builds (`-nightly.` tags),
#                       cut once a day from `main` when it has new commits
# Every channel skips releases without this platform's tarball.
#
# CLI-only by design: there is no env-var fallback. A user must pass
# `--channel beta` or `--channel nightly` explicitly to opt into prereleases.
# `bash install.sh` always lands on stable — same contract as `temps upgrade`.
channel="stable"
positional=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --channel=*)
            channel="${1#--channel=}"
            shift
            ;;
        --channel)
            shift
            [[ $# -gt 0 ]] || error '--channel requires a value, e.g. --channel beta'
            channel="$1"
            shift
            ;;
        *)
            positional+=("$1")
            shift
            ;;
    esac
done

case "$channel" in
    stable|beta|nightly) ;;
    *)
        error "Unknown channel '$channel'. Supported: stable, beta, nightly"
        ;;
esac

if [[ ${#positional[@]} -gt 1 ]]; then
    error 'Too many arguments. Usage: install.sh [--channel stable|beta|nightly] [version]'
fi

case $platform in
'Darwin x86_64')
    target=darwin-amd64
    ;;
'Darwin arm64')
    target=darwin-arm64
    ;;
'Linux aarch64' | 'Linux arm64')
    target=linux-arm64
    ;;
'Linux x86_64' | *)
    target=linux-amd64
    ;;
esac

GITHUB=${GITHUB-"https://github.com"}

github_repo="$GITHUB/gotempsh/temps"

exe_name=temps

asset_name="temps-$target.tar.gz"

if [[ ${#positional[@]} -eq 0 ]]; then
    info "Fetching latest release on channel: $channel"

    # Channel resolution against GitHub Releases. Every channel only ever
    # resolves to a version-shaped tag (`release_tag_pattern`, so `test-v*`
    # smoke-test releases are never candidates) that ships this platform's
    # tarball (`release_tags_with_asset`), mirroring `temps upgrade`.
    #
    # - stable: GET /releases/latest -- the release GitHub marks "Latest".
    #   The release workflow only marks a stable tag Latest when it is the
    #   highest stable version, so a backported patch on an older line never
    #   becomes the default install. 404 means there are zero stable releases
    #   yet; fall through to a helpful error.
    # - beta: /releases/latest skips prereleases, so walk /releases and take
    #   the highest version by semver that is NOT a nightly build (mirrors
    #   `UpgradeChannel::Beta`, which excludes `-nightly.` tags so a
    #   deliberate beta opt-in never silently resolves to an automated
    #   nightly). A stable release is a valid beta-channel result when it is
    #   the newest version.
    # - nightly: same listing, highest version that IS a nightly build
    #   (`-nightly.`), minted by the "Nightly Release" workflow.
    #
    # The listing is ordered by creation date, not version, so the highest
    # version is computed over every page fetched rather than taken from the
    # first match: a backfilled release must not shadow a newer one. Walk up
    # to 5 pages of 100 releases (500 releases of headroom), stopping at the
    # first short page.
    set +e
    temps_tag=""
    if [[ "$channel" = "stable" ]]; then
        github_api_get "https://api.github.com/repos/gotempsh/temps/releases/latest"
        case "$api_status" in
            200)
                # `grep -o` on the key itself, so minified JSON (one line, or
                # `"tag_name":"x"` with no space) parses the same as
                # pretty-printed JSON.
                temps_tag=$(printf '%s\n' "$api_body" |
                            grep -oE '"tag_name": *"[^"]*"' |
                            head -n 1 |
                            sed -E 's/.*"([^"]*)"$/\1/')
                if [[ -n "$temps_tag" && ! $temps_tag =~ $release_tag_pattern ]]; then
                    error "The latest stable release has an unexpected tag '$temps_tag'.
Pin a version instead:
    curl -fsSL https://raw.githubusercontent.com/gotempsh/temps/main/scripts/install.sh | bash -s -- <version>

Available versions: https://github.com/gotempsh/temps/releases"
                fi
                stable_installable=$(release_tags_with_asset "$api_body" "$asset_name")
                if [[ -n "$temps_tag" ]] &&
                   ! printf '%s\n' "$stable_installable" | grep -xF -- "$temps_tag" >/dev/null; then
                    error "The latest stable release $temps_tag has no $asset_name asset, so it cannot be
installed on this platform. Pin a version that ships one:
    curl -fsSL https://raw.githubusercontent.com/gotempsh/temps/main/scripts/install.sh | bash -s -- <version>

Available versions: https://github.com/gotempsh/temps/releases"
                fi
                ;;
            404)
                # No stable release has been published yet. Do NOT fall back
                # to a prerelease on the user's behalf: the default channel is
                # a promise that `bash install.sh` never installs a beta, so
                # say what to do instead and let them opt in explicitly.
                echo ""
                error "Temps has not published a stable release yet -- every release so far is a
beta or nightly prerelease, so the default 'stable' channel has nothing to install.

To install the newest beta, opt in explicitly:
    curl -fsSL https://raw.githubusercontent.com/gotempsh/temps/main/scripts/install.sh | bash -s -- --channel beta

Or pin a specific version:
    curl -fsSL https://raw.githubusercontent.com/gotempsh/temps/main/scripts/install.sh | bash -s -- <version>

Available versions: https://github.com/gotempsh/temps/releases"
                ;;
            *)
                github_api_error "the latest stable release"
                ;;
        esac
    else
        candidates=""
        page=1
        while [[ $page -le 5 ]]; do
            github_api_get "https://api.github.com/repos/gotempsh/temps/releases?per_page=100&page=$page"
            if [[ "$api_status" != "200" ]]; then
                # Page 1 failing means we know nothing. A later page failing
                # (typically the unauthenticated rate limit) still leaves the
                # newest releases, which is where the answer almost always is.
                if [[ $page -gt 1 && -n "$candidates" ]]; then
                    warning "Could not read page $page of the release list (HTTP $api_status); choosing from the first $(( (page - 1) * 100 )) releases."
                    break
                fi
                github_api_error "$channel releases"
            fi
            candidates+=$(release_tags_with_asset "$api_body" "$asset_name")$'\n'
            page_releases=$(printf '%s\n' "$api_body" | grep -oE '"tag_name": *"' | wc -l | tr -d ' ')
            [[ $page_releases -lt 100 ]] && break
            page=$((page + 1))
        done

        if [[ "$channel" = "nightly" ]]; then
            temps_tag=$(printf '%s' "$candidates" | grep -- '-nightly\.' | highest_semver_tag)
        else
            temps_tag=$(printf '%s' "$candidates" | grep -v -- '-nightly\.' | highest_semver_tag)
        fi
    fi
    set -e

    if [[ -z "$temps_tag" ]]; then
        echo ""
        error "No installable release found on channel '$channel' (a release must ship $asset_name).
Try a specific version:
    curl -fsSL https://raw.githubusercontent.com/gotempsh/temps/main/scripts/install.sh | bash -s -- v0.1.0

Or pick a different channel:
    curl -fsSL https://raw.githubusercontent.com/gotempsh/temps/main/scripts/install.sh | bash -s -- --channel beta

Available versions: https://github.com/gotempsh/temps/releases"
    fi

    info "Latest version on $channel: $temps_tag"
    if [[ "$channel" = "stable" ]]; then
        warn_if_stable_predates_beta "$temps_tag"
    fi
else
    # Explicit version pin -- channel is irrelevant. Accept `0.1.0` as well
    # as `v0.1.0` (release tags always carry the `v`), and reject anything
    # that is not version-shaped before it reaches a URL.
    temps_tag="v${positional[0]#v}"
    if [[ ! $temps_tag =~ $release_tag_pattern ]]; then
        error "'${positional[0]}' is not a release version. Expected something like
'v0.1.0', '0.1.0' or 'v0.1.0-rc.1'.

Usage: install.sh [--channel stable|beta|nightly] [version]
Available versions: https://github.com/gotempsh/temps/releases"
    fi
    info "Installing pinned version: $temps_tag"
fi
temps_uri=$github_repo/releases/download/$temps_tag/$asset_name

install_env=TEMPS_INSTALL
bin_env=\$$install_env/bin

install_dir=${!install_env:-$HOME/.temps}
bin_dir=$install_dir/bin
exe=$bin_dir/temps

if [[ ! -d $bin_dir ]]; then
    mkdir -p "$bin_dir" ||
        error "Failed to create install directory \"$bin_dir\""
fi

info "Downloading temps from $temps_uri..."

tarball="$install_dir/temps-$target.tar.gz"

curl --fail --location --progress-bar --output "$tarball" "$temps_uri" ||
    error "Failed to download temps from \"$temps_uri\""

info "Verifying download integrity..."

verify_checksum "$tarball" "$temps_uri.sha256"

info "Extracting temps..."

tar -xzf "$tarball" -C "$bin_dir" temps ||
    error "Failed to extract temps"

rm "$tarball" ||
    warning "Failed to remove temporary tarball"

chmod +x "$exe" ||
    error 'Failed to set permissions on temps executable'

tildify() {
    if [[ $1 = $HOME/* ]]; then
        local replacement=\~/

        echo "${1/$HOME\//$replacement}"
    else
        echo "$1"
    fi
}

success "temps was installed successfully to $Bold_Green$(tildify "$exe")"

if command -v temps >/dev/null; then
    echo "Run 'temps --help' to get started"
    exit
fi

refresh_command=''

tilde_bin_dir=$(tildify "$bin_dir")
quoted_install_dir=\"${install_dir//\"/\\\"}\"

if [[ $quoted_install_dir = \"$HOME/* ]]; then
    quoted_install_dir=${quoted_install_dir/$HOME\//\$HOME/}
fi

echo

case $(basename "$SHELL") in
fish)
    commands=(
        "set --export $install_env $quoted_install_dir"
        "set --export PATH $bin_env \$PATH"
    )

    fish_config=$HOME/.config/fish/config.fish
    tilde_fish_config=$(tildify "$fish_config")

    if [[ -w $fish_config ]]; then
        {
            echo -e '\n# temps'

            for command in "${commands[@]}"; do
                echo "$command"
            done
        } >>"$fish_config"

        info "Added \"$tilde_bin_dir\" to \$PATH in \"$tilde_fish_config\""

        refresh_command="source $tilde_fish_config"
    else
        echo "Manually add the directory to $tilde_fish_config (or similar):"

        for command in "${commands[@]}"; do
            info_bold "  $command"
        done
    fi
    ;;
zsh)
    commands=(
        "export $install_env=$quoted_install_dir"
        "export PATH=\"$bin_env:\$PATH\""
    )

    zsh_config=$HOME/.zshrc
    tilde_zsh_config=$(tildify "$zsh_config")

    if [[ -w $zsh_config ]]; then
        {
            echo -e '\n# temps'

            for command in "${commands[@]}"; do
                echo "$command"
            done
        } >>"$zsh_config"

        info "Added \"$tilde_bin_dir\" to \$PATH in \"$tilde_zsh_config\""

        refresh_command="exec $SHELL"
    else
        echo "Manually add the directory to $tilde_zsh_config (or similar):"

        for command in "${commands[@]}"; do
            info_bold "  $command"
        done
    fi
    ;;
bash)
    commands=(
        "export $install_env=$quoted_install_dir"
        "export PATH=\"$bin_env:\$PATH\""
    )

    bash_configs=(
        "$HOME/.bash_profile"
        "$HOME/.bashrc"
    )

    if [[ ${XDG_CONFIG_HOME:-} ]]; then
        bash_configs+=(
            "$XDG_CONFIG_HOME/.bash_profile"
            "$XDG_CONFIG_HOME/.bashrc"
            "$XDG_CONFIG_HOME/bash_profile"
            "$XDG_CONFIG_HOME/bashrc"
        )
    fi

    set_manually=true
    for bash_config in "${bash_configs[@]}"; do
        tilde_bash_config=$(tildify "$bash_config")

        if [[ -w $bash_config ]]; then
            {
                echo -e '\n# temps'

                for command in "${commands[@]}"; do
                    echo "$command"
                done
            } >>"$bash_config"

            info "Added \"$tilde_bin_dir\" to \$PATH in \"$tilde_bash_config\""

            refresh_command="source $bash_config"
            set_manually=false
            break
        fi
    done

    if [[ $set_manually = true ]]; then
        echo "Manually add the directory to $tilde_bash_config (or similar):"

        for command in "${commands[@]}"; do
            info_bold "  $command"
        done
    fi
    ;;
*)
    echo 'Manually add the directory to ~/.bashrc (or similar):'
    info_bold "  export $install_env=$quoted_install_dir"
    info_bold "  export PATH=\"$bin_env:\$PATH\""
    ;;
esac

echo
info "To get started, run:"
echo

if [[ $refresh_command ]]; then
    info_bold "  $refresh_command"
fi

info_bold "  temps --help"
