#!/bin/sh
# Install the pinned release. Review this file before running it.
# Keep execution at EOF so an incomplete download cannot start installation.
main() {
    set -eu
    version=v0.1.3
    skills=none
    bin_dir=${PROCLEASE_INSTALL_DIR:-"$HOME/.local/bin"}
    data_dir=${XDG_DATA_HOME:-"$HOME/.local/share"}/proclease
    while [ "$#" -gt 0 ]; do
        case "$1" in
            --skills)
                [ "$#" -ge 2 ] || { echo '--skills requires codex, claude, both or none' >&2; return 1; }
                skills=$2; shift 2 ;;
            --help)
                echo 'Usage: sh install.sh [--skills codex|claude|both|none]'
                echo 'Installs v0.1.3 to ~/.local/bin; PROCLEASE_INSTALL_DIR overrides the binary directory.'
                return 0 ;;
            *) echo "Unknown option: $1" >&2; return 1 ;;
        esac
    done
    case "$skills" in codex|claude|both|none) ;; *) echo 'Invalid --skills value' >&2; return 1 ;; esac
    case "$bin_dir:$data_dir" in /*:/*) ;; *) echo 'Install directories must be absolute' >&2; return 1 ;; esac
    case "$(uname -s)" in
        Darwin) platform=apple-darwin ;;
        Linux) platform=unknown-linux-gnu ;;
        *) echo 'ProcLease supports macOS and Linux' >&2; return 1 ;;
    esac
    case "$(uname -m)" in
        arm64|aarch64) arch=aarch64 ;;
        x86_64|amd64) arch=x86_64 ;;
        *) echo 'Supported architectures: arm64 and x86_64' >&2; return 1 ;;
    esac
    command -v curl >/dev/null || { echo 'curl is required' >&2; return 1; }
    if command -v sha256sum >/dev/null; then hash=sha256sum
    elif command -v shasum >/dev/null; then hash=shasum
    else echo 'sha256sum or shasum is required' >&2; return 1
    fi
    tmp=$(mktemp -d "${TMPDIR:-/tmp}/proclease-install.XXXXXXXX")
    trap 'rm -rf "$tmp"' EXIT
    trap 'exit 1' HUP INT TERM
    name=proclease-$version-$arch-$platform
    archive=$name.tar.gz
    base=https://github.com/jaredchu/proclease/releases/download/$version
    curl --proto '=https' --tlsv1.2 -fsSL "$base/$archive" -o "$tmp/$archive"
    curl --proto '=https' --tlsv1.2 -fsSL "$base/$archive.sha256" -o "$tmp/checksum"
    expected=$(awk -v file="$archive" '$2 == file {print $1}' "$tmp/checksum")
    [ "${#expected}" -eq 64 ] || { echo 'Invalid release checksum' >&2; return 1; }
    case "$expected" in *[!0-9a-f]*) echo 'Invalid release checksum' >&2; return 1 ;; esac
    if [ "$hash" = sha256sum ]; then actual=$(sha256sum "$tmp/$archive")
    else actual=$(shasum -a 256 "$tmp/$archive")
    fi
    [ "${actual%% *}" = "$expected" ] || { echo 'Release checksum mismatch; nothing installed' >&2; return 1; }
    tar -tzf "$tmp/$archive" > "$tmp/members"
    while IFS= read -r member; do
        case "$member" in /*|../*|*/../*|*/..) echo 'Unsafe archive path' >&2; return 1 ;; esac
        case "$member" in "$name"|"$name/"*) ;; *) echo 'Unexpected archive contents' >&2; return 1 ;; esac
    done < "$tmp/members"
    tar -xzf "$tmp/$archive" -C "$tmp"
    [ "$("$tmp/$name/proclease" --version)" = "proclease ${version#v}" ] || { echo 'Unexpected binary version' >&2; return 1; }
    mkdir -p "$bin_dir" "$data_dir/skills"
    # Stage beside the destination so the final replacement is atomic.
    install -m 755 "$tmp/$name/proclease" "$bin_dir/.proclease-install.$$"
    mv -f "$bin_dir/.proclease-install.$$" "$bin_dir/proclease"
    skill_path=$data_dir/skills/proclease-$version
    if [ ! -e "$skill_path" ] && [ ! -L "$skill_path" ]; then
        cp -R "$tmp/$name/skills/proclease" "$skill_path"
    fi
    for host in codex claude; do
        case "$skills:$host" in both:*|codex:codex|claude:claude) ;; *) continue ;; esac
        if [ "$host" = codex ]; then parent=$HOME/.agents/skills
        else parent=$HOME/.claude/skills
        fi
        mkdir -p "$parent"
        destination=$parent/proclease
        if [ -L "$destination" ]; then
            case "$(readlink "$destination")" in
                "$data_dir"/skills/proclease-v*) rm "$destination" ;;
            esac
        fi
        if [ -e "$destination" ] || [ -L "$destination" ]; then
            echo "Existing $host skill preserved: $destination"
        else ln -s "$skill_path" "$destination"
        fi
    done
    echo "Installed proclease ${version#v}: $bin_dir/proclease"
    case ":$PATH:" in *":$bin_dir:"*) ;; *) echo "Add $bin_dir to your PATH." ;; esac
    echo 'Restart your agent session to discover newly installed skills.'
}
main "$@"
