#!/usr/bin/env bash
# Install apt packages through a cache of the .deb files they download.
# Location: scripts/ci/apt-install-cached.sh
#
# Run by .github/actions/apt-install-cached, which restores and saves the cache
# around these subcommands.
#
# Why. linux-qt-host-kit's install is the largest apt list in PR CI. Once the
# install as a whole was no longer capped (stalls are bounded by
# Acquire::http::Timeout instead), a mirror serving every package at ~200 KB/s
# spent the job's whole 25 minutes downloading: #618's and #626's runs were
# cancelled before the contract they exist for had started. Those bytes are the
# same from run to run until the archive publishes new versions, so they are
# kept in the Actions cache rather than fetched again.
#
# The key is what apt would download, not the package list. `--print-uris`
# against an archive directory nothing has been restored into names every .deb
# the install needs, by file name (name_version_arch) and the index's hash. The
# key is a digest of those pairs, so it changes exactly when the bytes would --
# a key made of the package names alone would keep restoring last month's
# versions next to the ones apt now wants, and the install would download
# anyway while the cache said it hit. Mirror hosts are left out of the key:
# the same file arrives from azure.archive.ubuntu.com or archive.ubuntu.com.
#
# Subcommands, in the order the action runs them:
#   plan       refresh the indexes, list what the install would download, and
#              write `key` and `count` to $GITHUB_OUTPUT
#   check-hit  on an exact cache hit: fail unless apt now needs to download nothing
#   install    install the packages; prints apt's own "Need to get X/Y" line
#   prune      before saving: drop every .deb the plan did not name, and fail
#              if the install did not leave every one it did name
#
# Every subcommand reads the packages from MIGO_APT_PACKAGES (whitespace
# separated) and keeps its plan in MIGO_APT_PLAN.
set -euo pipefail

ARCHIVES=/var/cache/apt/archives

read -r -a PACKAGES <<<"${MIGO_APT_PACKAGES:?MIGO_APT_PACKAGES names the packages to install}"
PLAN="${MIGO_APT_PLAN:?MIGO_APT_PLAN names the file the plan is kept in}"
(( ${#PACKAGES[@]} > 0 )) || { echo "MIGO_APT_PACKAGES is empty" >&2; exit 2; }

# What apt would fetch for this install, as "file hash" lines in a stable order.
# A .deb already in $ARCHIVES with the index's hash is not listed: that is what
# makes the same query the proof that a restored cache is complete.
would_download() {
    sudo apt-get install -y -qq --print-uris "${PACKAGES[@]}" |
        awk '/^\x27/ { print $2, $4 }' | LC_ALL=C sort
}

plan() {
    # `update` is advisory. Against a mirror that is down, apt retries
    # internally and reports `Ign:` per index, then falls back to the indexes
    # already on the image -- from which these packages still install. Bounding
    # it and treating that as fatal turned a survivable mirror outage into a
    # failed job, which is what happened when azure.archive.ubuntu.com went
    # dark. Still bounded, so a hung mirror cannot sit on the runner.
    timeout 180 sudo apt-get update \
        || echo "apt-get update incomplete; continuing on the image's indexes" >&2

    would_download >"$PLAN"
    local count digest
    count="$(wc -l <"$PLAN" | tr -d ' ')"
    digest="$(sha256sum "$PLAN" | cut -c1-32)"
    echo "the install would download $count .deb file(s); plan digest $digest"

    # actions/cache restores as the runner user, and apt leaves the directory
    # root's. Only the directory changes hands: partial/ stays _apt's, so apt's
    # download sandbox is unaffected.
    sudo chown "$(id -un)" "$ARCHIVES"

    if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
        printf 'key=%s\ncount=%s\n' "$digest" "$count" >>"$GITHUB_OUTPUT"
    fi
}

check_hit() {
    local missing
    missing="$(would_download)"
    if [[ -n "$missing" ]]; then
        echo "::error::the cache hit on this exact plan, yet apt would still download:" >&2
        printf '%s\n' "$missing" >&2
        exit 1
    fi
    echo "cache hit: every .deb the plan names is in $ARCHIVES; nothing to download"
}

install() {
    # A transfer that stalls is given up after 30 s and retried (Acquire::*):
    # the mirror stalling on one package -- libcodec2, 9 MB -- once used a whole
    # 300 s install cap twice over. The install as a whole is not capped: a
    # mirror at ~200 KB/s delivers every package, just slowly, and a cap killed
    # the install and its retry at exactly that cap. Stalls are what a bound is
    # for; the job's ceiling bounds the rest. Keep-Downloaded-Packages is
    # apt-get's default and is stated because the cache is made of what it keeps.
    install_once() {
        sudo apt-get install -y \
            -o Acquire::Retries=5 -o Acquire::http::Timeout=30 \
            -o APT::Keep-Downloaded-Packages=true \
            "${PACKAGES[@]}"
    }
    install_once || {
        echo "package install failed; retrying once" >&2
        install_once
    }
}

prune() {
    local file kept=0 dropped=0 absent=()
    local -A planned=()
    while read -r file _; do
        planned[$file]=1
    done <"$PLAN"
    for file in "$ARCHIVES"/*.deb; do
        [[ -e "$file" ]] || continue
        if [[ -n "${planned[$(basename "$file")]:-}" ]]; then
            kept=$((kept + 1))
        else
            # Restored through a restore-key from an older plan, or on the image
            # before this job: saved again, the cache would grow by every
            # version the archive has ever published.
            sudo rm -f -- "$file"
            dropped=$((dropped + 1))
        fi
    done
    for file in "${!planned[@]}"; do
        [[ -f "$ARCHIVES/$file" ]] || absent+=("$file")
    done
    if (( ${#absent[@]} > 0 )); then
        # Saved like this, the next run would hit this key and then download
        # these anyway -- the slow path, wearing a hit.
        echo "::error::the install did not leave ${#absent[@]} planned .deb file(s) in $ARCHIVES:" >&2
        printf '  %s\n' "${absent[@]}" >&2
        exit 1
    fi
    echo "saving $kept .deb file(s) ($(du -ch "$ARCHIVES"/*.deb 2>/dev/null | tail -1 | cut -f1)); dropped $dropped not in the plan"
}

case "${1:-}" in
    plan) plan ;;
    check-hit) check_hit ;;
    install) install ;;
    prune) prune ;;
    *) echo "usage: $0 plan|check-hit|install|prune" >&2; exit 2 ;;
esac
