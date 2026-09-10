#!/bin/sh
# Install the project's Linux release as sv. Requires curl or wget.
set -eu

REPOSITORY=x1t/sv-rs
ASSET_PREFIX=sv-rs
REQUESTED_VERSION=${SV_VERSION:-latest}
INSTALL_DIR=${SV_INSTALL_DIR:-/usr/local/bin}

die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

usage() {
    printf '%s\n' \
        "Install $REPOSITORY from GitHub Releases as sv." \
        'Usage: install.sh [--version VERSION] [--install-dir DIR]' \
        '  --version VERSION  Release tag (default: latest; accepts 0.3.0 or v0.3.0)' \
        '  --install-dir DIR  Existing absolute directory (default: /usr/local/bin)' \
        '  -h, --help         Show help' \
        'Environment: SV_VERSION, SV_INSTALL_DIR'
}

has_command() {
    command -v "$1" >/dev/null 2>&1
}

download_to() {
    if has_command curl; then
        curl -fsSL --retry 3 --connect-timeout 10 --max-time 300 -o "$2" "$1"
    elif has_command wget; then
        wget -q -O "$2" "$1"
    else
        die 'curl or wget is required'
    fi
}

main() {
    while [ "$#" -gt 0 ]; do
        case $1 in
            --version)
                [ "$#" -ge 2 ] || die '--version requires a value'
                REQUESTED_VERSION=$2
                shift 2
                ;;
            --install-dir)
                [ "$#" -ge 2 ] || die '--install-dir requires a value'
                INSTALL_DIR=$2
                shift 2
                ;;
            -h|--help) usage; return 0 ;;
            *) die "unknown argument: $1" ;;
        esac
    done

    case $INSTALL_DIR in
        /*) ;;
        *) die '--install-dir must be an absolute path' ;;
    esac
    [ -d "$INSTALL_DIR" ] || die "install directory does not exist: $INSTALL_DIR"
    [ "$(uname -s)" = Linux ] || die 'only Linux is supported'
    case $(uname -m) in
        x86_64|amd64) arch=amd64 ;;
        aarch64|arm64) arch=arm64 ;;
        *) die "unsupported architecture: $(uname -m); supported: amd64, arm64" ;;
    esac
    case $REQUESTED_VERSION in
        latest) base_url="https://github.com/$REPOSITORY/releases/latest/download" ;;
        *)
            case $REQUESTED_VERSION in
                ''|*[!A-Za-z0-9._-]*) die 'invalid release version' ;;
                v*) ;;
                *) REQUESTED_VERSION=v$REQUESTED_VERSION ;;
            esac
            [ "$REQUESTED_VERSION" != v ] || die 'invalid release version'
            base_url="https://github.com/$REPOSITORY/releases/download/$REQUESTED_VERSION"
            ;;
    esac
    has_command install || die 'install is required'
    if [ "$(id -u)" -ne 0 ] && [ ! -w "$INSTALL_DIR" ]; then
        has_command sudo || die "write access to $INSTALL_DIR is required; run as root or use --install-dir"
        use_sudo=yes
    else
        use_sudo=no
    fi

    install_path=$INSTALL_DIR/sv
    [ ! -d "$install_path" ] || die "installation target is a directory: $install_path"
    [ ! -L "$install_path" ] || die "installation target is a symbolic link: $install_path"
    tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/sv-install.XXXXXX")
    cleanup() {
        if [ -f "$tmp_dir/sv" ]; then rm -- "$tmp_dir/sv"; fi
        rmdir -- "$tmp_dir"
    }
    trap cleanup 0
    trap 'exit 1' 1 2 3 15

    asset=$ASSET_PREFIX-linux-$arch
    printf 'downloading %s (%s): %s\n' "$REPOSITORY" "$REQUESTED_VERSION" "$asset"
    download_to "$base_url/$asset" "$tmp_dir/sv" || die "failed to download $asset"
    [ -s "$tmp_dir/sv" ] || die 'downloaded binary is empty'
    if [ "$use_sudo" = yes ]; then
        sudo install -m 0755 "$tmp_dir/sv" "$install_path"
    else
        install -m 0755 "$tmp_dir/sv" "$install_path"
    fi
    printf 'installed %s\n' "$install_path"
}

main "$@"
