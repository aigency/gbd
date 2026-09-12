#!/bin/sh
# gbd installer — POSIX sh, designed for `curl -fsSL <url> | sh`.
#
# Resolves OS/arch, downloads the matching release tarball + SHA256SUMS from
# GitHub Releases, verifies the release signature on SHA256SUMS when
# `minisign` is installed, verifies the tarball's checksum, extracts the `gbd`
# binary into ${GBD_INSTALL_DIR:-$HOME/.local/bin}, and writes a sentinel
# `.gbd-installed-by`.
#
# Fetch:
#   curl -fsSL https://raw.githubusercontent.com/aigency/gbd/main/scripts/install.sh | sh
#
# Downloads prefer `gh release download`, then curl with GH_TOKEN/GITHUB_TOKEN,
# then unauthenticated curl.
#
# Environment variables:
#   GBD_VERSION       Pin to a specific tag (e.g. v0.1.0). Defaults to latest.
#   GBD_INSTALL_DIR   Destination directory. Defaults to $HOME/.local/bin.
#   GH_TOKEN / GITHUB_TOKEN   Optional. Used if `gh` is missing.

set -eu

REPO="aigency/gbd"
RELEASES_API="https://api.github.com/repos/${REPO}/releases/latest"
RELEASES_DOWNLOAD="https://github.com/${REPO}/releases/download"
SOURCE_INSTALL_HINT="cargo install --locked gbd"
# Releases published before signing existed. Only these may install with a
# checksum and no signature; every later release must carry one.
UNSIGNED_RELEASES=" v1.0.0 "
# The release signing public key (minisign). Must match
# [package.metadata.binstall.signing] in Cargo.toml; a test checks that.
SIGNING_PUBKEY="RWTJfFNVFWOcQa3j8m8WBvpgOGO0qocEnMMt8UnIb0wqO0KLgvwb6Fi4"

usage() {
    cat <<'EOF'
gbd installer

Usage:
    curl -fsSL https://raw.githubusercontent.com/aigency/gbd/main/scripts/install.sh | sh

    # Cloud agents with gh already logged in can fetch the script the same way,
    # or through the API:
    gh api -H "Accept: application/vnd.github.raw" \
        repos/aigency/gbd/contents/scripts/install.sh | sh

    sh install.sh [--help]

Environment variables:
    GBD_VERSION       Pin to a specific release tag (e.g. v0.1.0).
                      Defaults to the latest GitHub Release.
    GBD_INSTALL_DIR   Destination directory for the `gbd` binary.
                      Defaults to $HOME/.local/bin.
    GH_TOKEN / GITHUB_TOKEN
                      Used for curl if `gh` is not on PATH.

Prefers `gh release download`, then authenticated curl, then unauthenticated
curl. Installs to ~/.local/bin/gbd.

Verification: the tarball's SHA256 is always checked against the release's
SHA256SUMS. If `minisign` is installed, SHA256SUMS itself is first verified
against the release signature (SHA256SUMS.minisig) and the project's public
key, which proves the sums came from this project's release workflow.
EOF
}

err() {
    printf 'error: %s\n' "$1" >&2
    exit 1
}

have_gh() {
    command -v gh >/dev/null 2>&1
}

# Token for curl. Never print this.
github_token() {
    if [ -n "${GH_TOKEN:-}" ]; then
        printf '%s' "${GH_TOKEN}"
        return 0
    fi
    if [ -n "${GITHUB_TOKEN:-}" ]; then
        printf '%s' "${GITHUB_TOKEN}"
        return 0
    fi
    if have_gh; then
        gh auth token 2>/dev/null || true
    fi
}

curl_github() {
    # curl_github URL output-file
    url="$1"
    out="$2"
    token=$(github_token)
    if [ -n "${token}" ]; then
        curl -fsSL \
            -H "Authorization: Bearer ${token}" \
            -H "Accept: application/octet-stream" \
            "${url}" -o "${out}"
    else
        curl -fsSL "${url}" -o "${out}"
    fi
}

# Resolve OS/arch into one of the three release-yml targets. Any other
# combination — including Intel Mac (Darwin-x86_64), which we deliberately
# don't publish a prebuilt for — falls through to the source-install hint.
detect_target() {
    os=$(uname -s)
    arch=$(uname -m)
    case "${os}-${arch}" in
        Darwin-arm64)        printf 'aarch64-apple-darwin' ;;
        Darwin-x86_64)
            err "Intel Mac (x86_64) is not in the prebuilt-binary matrix —
the macos-13 runner queue at GitHub is unbounded (often 30+ min) and the
Intel-Mac install base is small enough that we redirect to source build
instead. Install via:
    ${SOURCE_INSTALL_HINT}
This works on Intel Macs and produces a native binary."
            ;;
        Linux-x86_64)        printf 'x86_64-unknown-linux-gnu' ;;
        Linux-aarch64)       printf 'aarch64-unknown-linux-gnu' ;;
        *)
            err "unsupported platform: ${os}/${arch}.
gbd publishes prebuilt binaries for macOS arm64 (Apple Silicon) and Linux (x86_64, aarch64).
For other platforms install from source:
    ${SOURCE_INSTALL_HINT}"
            ;;
    esac
}

# Default: latest tag from GitHub Releases. Override: GBD_VERSION.
resolve_version() {
    if [ -n "${GBD_VERSION:-}" ]; then
        printf '%s' "${GBD_VERSION}"
        return
    fi
    tag=""
    if have_gh; then
        tag=$(gh release list -R "${REPO}" --limit 1 --json tagName --jq '.[0].tagName // empty' 2>/dev/null || true)
    fi
    if [ -z "${tag}" ]; then
        json=$(mktemp 2>/dev/null || mktemp -t gbd-rel)
        if curl_github "${RELEASES_API}" "${json}"; then
            tag=$(grep -oE '"tag_name":[[:space:]]*"v[^"]+"' "${json}" | head -1 | cut -d'"' -f4)
        fi
        rm -f "${json}"
    fi
    if [ -z "${tag}" ]; then
        err "could not resolve a GitHub Release for ${REPO}.
Cloud agents: this script needs a published release (merge to main after CI),
or install from source:
    ${SOURCE_INSTALL_HINT}
To pin: GBD_VERSION=vX.Y.Z"
    fi
    printf '%s' "${tag}"
}

download_assets() {
    # $1=tmpdir $2=version $3=tarball
    tmpdir="$1"
    version="$2"
    tarball="$3"

    if have_gh; then
        if gh release download -R "${REPO}" "${version}" \
                -p "${tarball}" -p SHA256SUMS -D "${tmpdir}"; then
            # Signature is best-effort at download time: releases before
            # signing was added do not have one. verify_signature decides.
            gh release download -R "${REPO}" "${version}" \
                -p SHA256SUMS.minisig -D "${tmpdir}" 2>/dev/null || true
            return 0
        fi
        printf 'warning: gh release download failed; trying curl\n' >&2
    fi

    tarball_url="${RELEASES_DOWNLOAD}/${version}/${tarball}"
    sums_url="${RELEASES_DOWNLOAD}/${version}/SHA256SUMS"
    printf 'downloading %s\n' "${tarball_url}"
    if ! curl_github "${tarball_url}" "${tmpdir}/${tarball}"; then
        err "failed to download ${tarball_url}
If ${REPO} is private, use gh (already logged in on cloud agents):
    gh api -H \"Accept: application/vnd.github.raw\" repos/${REPO}/contents/scripts/install.sh | sh
Or from source:
    ${SOURCE_INSTALL_HINT}"
    fi
    printf 'downloading SHA256SUMS\n'
    if ! curl_github "${sums_url}" "${tmpdir}/SHA256SUMS"; then
        err "failed to download ${sums_url}"
    fi
    curl_github "${sums_url}.minisig" "${tmpdir}/SHA256SUMS.minisig" 2>/dev/null || true
}

# Verify SHA256SUMS against the release signature. Hard failure on a bad
# signature, and on a missing signature for any release that should have
# one: a stripped signature must not downgrade to checksum-only. Only the
# releases in UNSIGNED_RELEASES may proceed without. A missing `minisign`
# is a one-line note.
verify_signature() {
    # $1=tmpdir $2=version
    if ! command -v minisign >/dev/null 2>&1; then
        printf 'note: install minisign to also verify the release signature\n'
        return 0
    fi
    if [ ! -s "$1/SHA256SUMS.minisig" ]; then
        case "${UNSIGNED_RELEASES}" in
            *" $2 "*)
                printf 'warning: %s predates release signing; checksum only\n' "$2" >&2
                return 0
                ;;
        esac
        err "release $2 should be signed but SHA256SUMS.minisig is missing; refusing to install"
    fi
    printf 'verifying release signature\n'
    (cd "$1" && minisign -V -q -P "${SIGNING_PUBKEY}" -x SHA256SUMS.minisig -m SHA256SUMS) \
        || err "release signature verification FAILED for SHA256SUMS; not installing"
}

detect_sha_verifier() {
    if command -v shasum >/dev/null 2>&1; then
        printf 'shasum'
    elif command -v sha256sum >/dev/null 2>&1; then
        printf 'sha256sum'
    else
        err "no sha256 verifier found (need \`shasum\` or \`sha256sum\`)."
    fi
}

verify_sha256() {
    case "$1" in
        shasum)    shasum -a 256 -c "$2" >/dev/null ;;
        sha256sum) sha256sum -c "$2" >/dev/null ;;
    esac
}

main() {
    case "${1:-}" in
        -h|--help) usage; exit 0 ;;
    esac

    for tool in tar uname grep cut head mktemp chmod mkdir; do
        if ! command -v "${tool}" >/dev/null 2>&1; then
            err "required tool not found: ${tool}"
        fi
    done
    if ! have_gh && ! command -v curl >/dev/null 2>&1; then
        err "need \`gh\` or \`curl\` to download the release"
    fi

    target=$(detect_target)
    version=$(resolve_version)
    verifier=$(detect_sha_verifier)

    destdir="${GBD_INSTALL_DIR:-${HOME}/.local/bin}"
    dest="${destdir}/gbd"
    sentinel="${destdir}/.gbd-installed-by"

    if [ -e "${dest}" ] && [ ! -e "${sentinel}" ]; then
        err "found a non-gbd-managed binary at ${dest}; remove it first or set GBD_INSTALL_DIR to a different location"
    fi

    if [ -x "${dest}" ] \
            && [ -e "${sentinel}" ] \
            && grep -qx "version=${version}" "${sentinel}" 2>/dev/null; then
        printf 'gbd %s already installed at %s\n' "${version}" "${dest}"
        printf 'GBD_BIN=%s\n' "${dest}"
        exit 0
    fi

    tarball="gbd-${version}-${target}.tar.gz"

    tmpdir=$(mktemp -d 2>/dev/null || mktemp -d -t gbd-install)
    if [ -z "${tmpdir}" ] || [ ! -d "${tmpdir}" ]; then
        err "could not create temp directory"
    fi
    trap 'rm -rf "${tmpdir}"' EXIT INT TERM HUP

    download_assets "${tmpdir}" "${version}" "${tarball}"
    verify_signature "${tmpdir}" "${version}"

    if ! grep " ${tarball}$" "${tmpdir}/SHA256SUMS" > "${tmpdir}/SHA256SUMS.expected"; then
        err "SHA256SUMS does not contain an entry for ${tarball}"
    fi

    printf 'verifying checksum\n'
    (cd "${tmpdir}" && verify_sha256 "${verifier}" SHA256SUMS.expected) \
        || err "checksum verification failed for ${tarball}"

    tarball_sha=$(cut -d' ' -f1 "${tmpdir}/SHA256SUMS.expected")

    mkdir -p "${destdir}"
    if ! tar -xzf "${tmpdir}/${tarball}" -C "${destdir}"; then
        err "failed to extract ${tarball} into ${destdir}"
    fi
    chmod +x "${dest}"

    cat > "${sentinel}" <<EOF
version=${version}
tarball_sha256=${tarball_sha}
target=${target}
EOF

    printf '✓ gbd %s installed to %s\n' "${version}" "${dest}"
    printf 'GBD_BIN=%s\n' "${dest}"

    case ":${PATH}:" in
        *":${destdir}:"*)
            printf 'gbd ping to start\n'
            ;;
        *)
            # Agents: paste this into the same session. Humans: also add to rc.
            printf 'this session:\n    export PATH="%s:$PATH"\n' "${destdir}"
            shell_name=$(basename "${SHELL:-}")
            # shellcheck disable=SC2088
            case "${shell_name}" in
                zsh)  rc='~/.zshrc' ;;
                bash) rc='~/.bashrc (or ~/.bash_profile on macOS)' ;;
                fish) rc='~/.config/fish/config.fish' ;;
                *)    rc='' ;;
            esac
            if [ -n "${rc}" ]; then
                # shellcheck disable=SC2016
                printf 'add the same line to %s for next time\n' "${rc}"
            fi
            ;;
    esac
}

main "$@"
