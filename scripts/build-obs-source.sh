#!/usr/bin/env bash
# Makes the files for the openSUSE package in the Open Build Service (OBS):
# target/obs/ gets the release tarball, the crates tarball (OBS builds
# offline) and packaging/opensuse/laccy.spec with Version set from
# Cargo.toml, and laccy.changes. The version's tag must exist.
#
#   scripts/build-obs-source.sh          make the files
#   scripts/build-obs-source.sh --test   also build the RPMs in an openSUSE
#                                        Tumbleweed container with no
#                                        network, like OBS, and run rpmlint
#
# Uploading is left to you (see the end of this script's output).
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
image=registry.opensuse.org/opensuse/tumbleweed:latest
test_image=localhost/laccy-obs-build:tumbleweed

test_build=false
for arg in "$@"; do
    case $arg in
        --test) test_build=true ;;
        *) echo "Unknown option $arg" >&2; exit 1 ;;
    esac
done

version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -n1)
tag=v$version
git -C "$root" rev-parse -q --verify "refs/tags/$tag" >/dev/null ||
    { echo "No tag $tag; release first." >&2; exit 1; }

out=$root/target/obs
mkdir -p "$out"
tarball=$out/laccy-$version.tar.gz
vendor=$out/vendor.tar.xz

# The tarball GitHub serves for the tag, so it matches the spec's Source0 URL.
if [[ ! -f $tarball ]]; then
    echo "Downloading $(basename "$tarball")"
    curl -fsSL -o "$tarball.part" "https://github.com/mlkonrad/laccy/archive/$tag/laccy-$version.tar.gz"
    mv "$tarball.part" "$tarball"
fi

# vendor.tar.xz has no version in its name, so remake it when the version
# changes. The stamp records which version it was made for.
if [[ ! -f $vendor || $(cat "$vendor.version" 2>/dev/null) != "$version" ]]; then
    echo "Making $(basename "$vendor") (cargo vendor)"
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT
    tar -C "$tmp" -xzf "$tarball"
    (cd "$tmp/laccy-$version" && cargo vendor --locked --versioned-dirs vendor >/dev/null)
    tag_time=$(git -C "$root" log -1 --format=%ct "$tag")
    tar -C "$tmp/laccy-$version" --sort=name --mtime="@$tag_time" \
        --owner=0 --group=0 --numeric-owner -cJf "$vendor" vendor
    echo "$version" >"$vendor.version"
    rm -rf "$tmp"
    trap - EXIT
fi

sed "s/^Version:\( *\).*/Version:\1$version/" "$root/packaging/opensuse/laccy.spec" >"$out/laccy.spec"
echo "Wrote laccy.spec for $version"

# OBS turns laccy.changes into the RPM's %changelog. A version without an
# entry gets one pointing to its release notes, dated like the tag.
changes=$root/packaging/opensuse/laccy.changes
if grep -q "version $version:\$\|Update to $version:\$" "$changes"; then
    cp "$changes" "$out/laccy.changes"
else
    tag_time=$(git -C "$root" log -1 --format=%ct "$tag")
    trailer=$(sed -n '2s/^.* - \(.*>\)$/\1/p' "$changes")
    {
        echo "-------------------------------------------------------------------"
        echo "$(LC_ALL=C TZ=UTC date -d "@$tag_time" '+%a %b %e %H:%M:%S UTC %Y') - $trailer"
        echo
        echo "- Update to $version:"
        echo "  https://github.com/mlkonrad/laccy/releases/tag/$tag"
        echo
        cat "$changes"
    } >"$out/laccy.changes"
    echo "Added a laccy.changes entry for $version"
fi

if $test_build; then
    if ! podman image exists "$test_image"; then
        echo "Making $test_image with the build dependencies"
        podman build -t "$test_image" -f - "$out" <<EOF
FROM $image
COPY laccy.spec /tmp/laccy.spec
RUN zypper -n install --no-recommends rpm-build rpmlint && \
    rpmspec -q --buildrequires /tmp/laccy.spec | tr -d ' ' | \
        xargs zypper -n install --no-recommends && \
    zypper clean -a
EOF
    fi
    echo "Building the RPMs with no network, like OBS"
    rm -rf "$out/test" && mkdir -p "$out/test"
    podman run --rm --network=none -v "$out:/build:z" "$test_image" bash -euc '
        rpmbuild -ba /build/laccy.spec \
            --define "_sourcedir /build" --define "_topdir /tmp/rpmbuild"
        cp /tmp/rpmbuild/RPMS/*/*.rpm /tmp/rpmbuild/SRPMS/*.rpm /build/test/
        rpmlint /build/test/*.rpm || true
    '
    echo "Test RPMs in $out/test"
fi

cat <<EOF

OBS files ready in $out:
  laccy.spec  laccy.changes  laccy-$version.tar.gz  vendor.tar.xz

To upload (Fedora: sudo dnf install osc), from a checkout of the package:
  osc checkout home:mlkonrad laccy && cd home:mlkonrad/laccy
  cp $out/{laccy.spec,laccy.changes,laccy-$version.tar.gz,vendor.tar.xz} .
  osc addremove && osc commit -m "Update to $version"
EOF
