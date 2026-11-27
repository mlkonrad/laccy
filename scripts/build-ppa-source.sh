#!/usr/bin/env bash
# Builds the Ubuntu source package for the Launchpad PPA, in an Ubuntu
# container (podman), from the release tag v<version>.
#
#   scripts/build-ppa-source.sh             # version from Cargo.toml
#   scripts/build-ppa-source.sh --test      # also build the .debs, offline
#   scripts/build-ppa-source.sh --in-ubuntu # already in Ubuntu 26.04 (CI):
#                                           # run the Ubuntu steps directly
#
# If packaging/debian/changelog has no entry for this version, the source
# package gets a "New upstream release" entry <version>-1~ppa1, so a release
# needs no packaging change. Add an entry by hand for anything else, such as
# a packaging fix released as -1~ppa2.
#
# Output goes to target/ppa/. Launchpad builds without network access, so
# the crates are bundled as a second tarball, laccy_<version>.orig-vendor.tar.xz.
# Both tarballs are made once per version, or taken from the PPA if it
# already has that version: Launchpad rejects a different tarball under a
# name it has already seen.
#
# Signing and uploading are left to you (see the end of this script's
# output), or to .github/workflows/ppa.yml for releases.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
image=docker.io/library/ubuntu:26.04
test_image=localhost/laccy-ppa-build:26.04

test_build=false
in_ubuntu=false
for arg in "$@"; do
    case $arg in
        --test) test_build=true ;;
        --in-ubuntu) in_ubuntu=true ;;
        *) echo "Unknown option $arg" >&2; exit 1 ;;
    esac
done
$test_build && $in_ubuntu && { echo "--test needs podman; drop --in-ubuntu." >&2; exit 1; }

version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -n1)
tag=v$version
git -C "$root" rev-parse -q --verify "refs/tags/$tag" >/dev/null ||
    { echo "No tag $tag; release first." >&2; exit 1; }

changelog=$root/packaging/debian/changelog
deb_version=$(sed -n '1s/^laccy (\([^)]*\)).*/\1/p' "$changelog")
new_entry=false
if [[ $deb_version != "$version"-* ]]; then
    new_entry=true
    deb_version=$version-1~ppa1
fi
tag_time=$(git -C "$root" log -1 --format=%ct "$tag")

out=$root/target/ppa
mkdir -p "$out"
orig=$out/laccy_$version.orig.tar.xz
vendor=$out/laccy_$version.orig-vendor.tar.xz
tar_opts=(--sort=name --mtime="@$tag_time"
    --owner=0 --group=0 --numeric-owner)

# A version already in the PPA keeps its tarballs: Launchpad rejects new
# ones under the same name, and the crates tarball isn't byte-for-byte
# reproducible across cargo versions. So take them from the PPA if it has them.
pool=https://ppa.launchpadcontent.net/mlkonrad/laccy/ubuntu/pool/main/l/laccy
for file in "$orig" "$vendor"; do
    if [[ ! -f $file ]] && curl -fsSL -o "$file.part" "$pool/$(basename "$file")"; then
        mv "$file.part" "$file"
        echo "Took $(basename "$file") from the PPA"
    fi
    rm -f "$file.part"
done

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

if $new_entry; then
    # Signed off by whoever signed the newest entry, dated like the tag.
    trailer=$(sed -n '0,/^ -- /s/^ -- \(.*>\)  .*/\1/p' "$changelog")
    {
        printf 'laccy (%s) resolute; urgency=medium\n\n' "$deb_version"
        printf '  * New upstream release:\n'
        printf '    https://github.com/mlkonrad/laccy/releases/tag/%s\n\n' "$tag"
        printf ' -- %s  %s\n\n' "$trailer" "$(LC_ALL=C date -R -d "@$tag_time")"
        cat "$changelog"
    } >"$work/debian/changelog"
    echo "Added changelog entry $deb_version"
fi

echo "Building the source package"
if $in_ubuntu; then
    (cd "$work" && dpkg-buildpackage -S -sa -d -us -uc)
else
    podman run --rm -v "$out:/build:z" -w "/build/laccy-$version" "$image" bash -euc '
        export DEBIAN_FRONTEND=noninteractive
        apt-get update -qq
        apt-get install -qq -y --no-install-recommends dpkg-dev debhelper >/dev/null
        dpkg-buildpackage -S -sa -d -us -uc
    '
fi

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
