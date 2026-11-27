#!/usr/bin/env bash
# Builds the Ubuntu source package for the Launchpad PPA, in an Ubuntu
# container (podman), from the release tag v<version>.
#
#   scripts/build-ppa-source.sh            # version from Cargo.toml
#   scripts/build-ppa-source.sh --test     # also build the .debs, offline
#
# Output goes to target/ppa/. Launchpad builds without network access, so
# the crates are bundled as a second tarball, laccy_<version>.orig-vendor.tar.xz.
# Both tarballs are made once per version and reused after that: Launchpad
# rejects a different tarball under a version it has already seen.
#
# Signing and uploading are left to you (see the end of this script's output).
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
image=docker.io/library/ubuntu:26.04
test_image=localhost/laccy-ppa-build:26.04

test_build=false
[[ ${1:-} == --test ]] && test_build=true

version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -n1)
tag=v$version
git -C "$root" rev-parse -q --verify "refs/tags/$tag" >/dev/null ||
    { echo "No tag $tag; release first." >&2; exit 1; }

deb_version=$(sed -n '1s/^laccy (\([^)]*\)).*/\1/p' "$root/packaging/debian/changelog")
[[ $deb_version == "$version"-* ]] ||
    { echo "packaging/debian/changelog is at $deb_version, not $version; add an entry." >&2; exit 1; }

out=$root/target/ppa
mkdir -p "$out"
orig=$out/laccy_$version.orig.tar.xz
vendor=$out/laccy_$version.orig-vendor.tar.xz
tar_opts=(--sort=name --mtime="@$(git -C "$root" log -1 --format=%ct "$tag")"
    --owner=0 --group=0 --numeric-owner)

if [[ ! -f $orig ]]; then
    echo "Making $(basename "$orig") from $tag"
    git -C "$root" archive --format=tar --prefix="laccy-$version/" "$tag" | xz -6 >"$orig"
fi

if [[ ! -f $vendor ]]; then
    echo "Making $(basename "$vendor") (cargo vendor)"
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT
    tar -C "$tmp" -xJf "$orig"
    (cd "$tmp/laccy-$version" && cargo vendor --locked --versioned-dirs vendor >/dev/null)
    tar -C "$tmp/laccy-$version" "${tar_opts[@]}" -cJf "$vendor" vendor
    rm -rf "$tmp"
    trap - EXIT
fi

# The source tree: the tag's files, the crates, and the packaging from this
# checkout, so packaging fixes don't need a new release.
work=$out/laccy-$version
rm -rf "$work"
tar -C "$out" -xJf "$orig"
tar -C "$work" -xJf "$vendor"
rm -rf "$work/debian"
cp -r "$root/packaging/debian" "$work/debian"

echo "Building the source package in $image"
podman run --rm -v "$out:/build:z" -w "/build/laccy-$version" "$image" bash -euc '
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq
    apt-get install -qq -y --no-install-recommends dpkg-dev debhelper >/dev/null
    dpkg-buildpackage -S -sa -d -us -uc
'

if $test_build; then
    if ! podman image exists "$test_image"; then
        echo "Making $test_image with the build dependencies"
        podman build -t "$test_image" -f - "$root/packaging/debian" <<EOF
FROM $image
COPY control /tmp/control
RUN apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -qq -y \
    --no-install-recommends dpkg-dev devscripts equivs lintian && \
    mk-build-deps -i -r -t 'apt-get -y --no-install-recommends' /tmp/control && \
    rm -rf /var/lib/apt/lists/*
EOF
    fi
    echo "Building the .debs with no network, like Launchpad"
    rm -rf "$out/test" && mkdir -p "$out/test"
    podman run --rm --network=none -v "$out:/build:z" -w /build/test "$test_image" bash -euc "
        dpkg-source -x ../laccy_$deb_version.dsc src >/dev/null
        cd src && dpkg-buildpackage -b -us -uc
        cd .. && lintian --info --tag-display-limit 0 *.changes || true
    "
    echo "Test .debs in $out/test"
fi

cat <<EOF

Source package ready in $out:
  laccy_${deb_version}_source.changes

To upload (once your Launchpad account has your GPG key and the PPA exists),
sign on this machine (Fedora: sudo dnf install devscripts), then upload with
dput from the Ubuntu container (-u: the signature is checked by Launchpad):
  debsign -k <your key id> $out/laccy_${deb_version}_source.changes
  podman run --rm -v "$out:/build:z" $image bash -c \\
    'apt-get update -qq && apt-get install -qq -y dput >/dev/null &&
     dput -u ppa:mlkonrad/laccy /build/laccy_${deb_version}_source.changes'
EOF
