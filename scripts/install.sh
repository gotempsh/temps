#!/usr/bin/env bash
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

command -v curl >/dev/null ||
    error 'curl is required to install temps'

# Channel selection. Mirrors `temps upgrade --channel`:
#   stable (default) — track non-prerelease tags only
#   beta             — track the newest tag, prerelease or not
#
# CLI-only by design: there is no env-var fallback. A user must pass
# `--channel beta` explicitly to opt into prereleases. `bash install.sh`
# always lands on stable — same contract as `temps upgrade`.
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
    stable|beta) ;;
    *)
        error "Unknown channel '$channel'. Supported: stable, beta"
        ;;
esac

if [[ ${#positional[@]} -gt 1 ]]; then
    error 'Too many arguments. Usage: install.sh [--channel beta|stable] [version]'
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

if [[ ${#positional[@]} -eq 0 ]]; then
    info "Fetching latest release on channel: $channel"

    # Channel resolution against GitHub Releases:
    #
    # - stable: GET /releases/latest returns the most recent NON-prerelease
    #   release. This is GitHub's contract — it's exactly what we want.
    #   404 means there are zero stable releases yet; fall through to a
    #   helpful error.
    # - beta: /releases/latest skips betas, so we walk the first page of
    #   /releases (newest-first) and take the very first `tag_name`.
    #
    # Why "first tag_name" (no draft check):
    #   We don't ship draft releases publicly — anything visible on the
    #   API is intended to be installable. Filtering drafts inside a
    #   shell script is brittle (tag_name and draft fields aren't in a
    #   guaranteed order across responses; awk pairing them requires a
    #   real JSON parser). The Rust CLI does check `draft` because it has
    #   serde; the bash installer trusts that the API only returns
    #   shipped releases.
    set +e
    if [[ "$channel" = "stable" ]]; then
        temps_tag=$(curl --silent "https://api.github.com/repos/gotempsh/temps/releases/latest" |
                    grep '"tag_name":' |
                    head -n 1 |
                    sed -E 's/.*"([^"]+)".*/\1/' 2>/dev/null)
    else
        # GitHub orders releases newest-first, so the first `tag_name`
        # in the page is the newest release of any kind.
        temps_tag=$(curl --silent "https://api.github.com/repos/gotempsh/temps/releases?per_page=20" |
                    grep '"tag_name":' |
                    head -n 1 |
                    sed -E 's/.*"([^"]+)".*/\1/' 2>/dev/null)
    fi
    set -e

    if [[ -z "$temps_tag" ]]; then
        echo ""
        error "No releases found on channel '$channel'. Try a specific version:
    curl -fsSL https://raw.githubusercontent.com/gotempsh/temps/main/scripts/install.sh | bash -s -- v0.1.0

Or pick a different channel:
    curl -fsSL https://raw.githubusercontent.com/gotempsh/temps/main/scripts/install.sh | bash -s -- --channel beta

Available versions: https://github.com/gotempsh/temps/releases"
    fi

    info "Latest version on $channel: $temps_tag"
    temps_uri=$github_repo/releases/download/$temps_tag/temps-$target.tar.gz
else
    # Explicit version pin — channel is irrelevant.
    temps_uri=$github_repo/releases/download/${positional[0]}/temps-$target.tar.gz
fi

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

info "Extracting temps..."

tar -xzf "$tarball" -C "$bin_dir" ||
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
