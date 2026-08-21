#!/bin/bash
#
# Build the thing other people download.
#
# scripts/bundle.sh makes an application that runs on this machine: signed with
# a certificate that exists in one keychain and nowhere else, which is enough
# for the Dock and the Keychain and nothing further. A copy of that bundle
# fetched from the internet does not open. It arrives quarantined, Gatekeeper
# looks for an authority behind the signature and finds a stranger, and what
# the person is told is that the application is damaged — which is a worse
# first impression than offering no download at all.
#
# So this is a second pipeline rather than a flag on that one, and every step
# in it exists because Gatekeeper wants it:
#
#   universal binary   a download is not allowed to ask what kind of Mac it
#                      landed on
#   Developer ID       a certificate whose chain leads back to Apple
#   hardened runtime   refused at notarization without it
#   secure timestamp   so the signature outlives the certificate that made it
#   notarized          Apple's own scan; its ticket is what Gatekeeper wants
#   stapled            the ticket attached to the image, so the first launch
#                      works on a machine that is offline
#
# Usage:
#   scripts/release.sh [--arch universal|arm64] [--allow-dirty]
#                      [--skip-notarize] [--publish]
#
# Environment:
#   TUPLI_RELEASE_IDENTITY  the Developer ID Application certificate to sign
#                           with (default: the only one in the keychain)
#   TUPLI_NOTARY_PROFILE    notarytool's keychain profile (default: tupli-notary)
#
# The notary profile is a one-time interactive step, because it takes an
# app-specific password and this script never sees one:
#
#   xcrun notarytool store-credentials tupli-notary \
#     --apple-id you@example.com --team-id XXXXXXXXXX --password <app-specific>

set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
arch=universal
publish=0
notarize=1
allow_dirty=0

while [ $# -gt 0 ]; do
  case $1 in
    --arch) arch=${2:?--arch needs a value}; shift 2 ;;
    --arch=*) arch=${1#*=}; shift ;;
    --publish) publish=1; shift ;;
    --skip-notarize) notarize=0; shift ;;
    --allow-dirty) allow_dirty=1; shift ;;
    -h|--help) sed -n '2,44p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "release: unknown argument: $1" >&2; exit 2 ;;
  esac
done

say() { echo "release: $*"; }
die() { echo "release: $*" >&2; exit 1; }

# ---------------------------------------------------------------- preflight
#
# Every check here is one that costs seconds now and a withdrawn download
# later. A release that fails halfway has already been a release.

version=$(sed -n '/^\[workspace.package\]/,/^\[/p' "$root/Cargo.toml" \
  | sed -n 's/^version *= *"\(.*\)"/\1/p' | head -1)
[ -n "$version" ] || die "could not read the workspace version"

if [ "$allow_dirty" = 0 ]; then
  # What ships has to be a commit somebody can check out again. An uncommitted
  # tree produces a binary with no name for where it came from.
  git -C "$root" diff --quiet && git -C "$root" diff --cached --quiet \
    || die "the tree has uncommitted changes. Commit them, or --allow-dirty."
fi

# Found rather than named, so the usual case needs no configuration — but not
# guessed at. The team the certificate belongs to is the publisher Gatekeeper
# names, and picking the wrong one of two is a mistake nobody sees until it has
# been downloaded.
identity=${TUPLI_RELEASE_IDENTITY:-}
if [ -z "$identity" ]; then
  found=$(security find-identity -v -p codesigning 2>/dev/null \
    | sed -n 's/.*"\(Developer ID Application: .*\)"/\1/p')
  count=$(printf '%s' "$found" | grep -c . || true)
  [ "$count" -le 1 ] || die "more than one Developer ID Application certificate:
$(printf '%s' "$found" | sed 's/^/          /')
        Name the one to sign with in TUPLI_RELEASE_IDENTITY."
  identity=$found
fi
[ -n "$identity" ] || die "no Developer ID Application certificate in the keychain.
        Xcode > Settings > Accounts > Manage Certificates > + > Developer ID Application,
        or set TUPLI_RELEASE_IDENTITY to one that is there under another name."

# Asked of notarytool rather than of the keychain. store-credentials writes to
# the data protection keychain, which the `security` tool cannot see at all, so
# looking there answers "no" no matter what is stored. This costs a second and
# a round trip and proves the stronger thing anyway: not that an item exists,
# but that Apple accepts it.
profile=${TUPLI_NOTARY_PROFILE:-tupli-notary}
if [ "$notarize" = 1 ]; then
  if ! probe=$(xcrun notarytool history --keychain-profile "$profile" 2>&1); then
    case $probe in
      *"No Keychain password item"*)
        die "no notarytool profile called '$profile'. Create it once:
          xcrun notarytool store-credentials $profile \\
            --apple-id you@example.com --team-id XXXXXXXXXX --password <app-specific>
        An app-specific password comes from appleid.apple.com, not your Apple ID password." ;;
      # Anything else is Apple saying no, or the network being down. Its own
      # words beat a guess at which.
      *) die "notarytool would not accept the '$profile' profile:
$(printf '%s' "$probe" | sed 's/^/          /')" ;;
    esac
  fi
fi

case $arch in
  universal) targets=(aarch64-apple-darwin x86_64-apple-darwin) ;;
  arm64)     targets=(aarch64-apple-darwin) ;;
  *) die "unknown --arch: $arch. Expected universal or arm64." ;;
esac
for target in "${targets[@]}"; do
  rustup target list --installed | grep -qx "$target" \
    || die "the $target toolchain is missing: rustup target add $target"
done

say "Tupli $version, $arch, signed as: $identity"

# --------------------------------------------------------------- the binary
#
# Built per architecture and joined with lipo, rather than left to whichever
# Mac happens to run this script. An arm64-only download on an Intel Mac is a
# dialog saying the application is not supported on this kind of computer, and
# the person reading it did nothing wrong.

for target in "${targets[@]}"; do
  say "building $target"
  (cd "$root" && cargo build --release --bin tupli -p tupli --target "$target")
done

fat="$root/target/release-dist/tupli"
mkdir -p "$(dirname "$fat")"
slices=()
for target in "${targets[@]}"; do slices+=("$root/target/$target/release/tupli"); done
lipo -create -output "$fat" "${slices[@]}"
say "$(lipo -archs "$fat")"

# --------------------------------------------------------------- the bundle
#
# bundle.sh owns the shape of a .app — the plist, the icon, the name and the
# identifier, all of which have to agree — so it builds this one too, and only
# the signature is done differently. --no-sign because the signature it applies
# is the local one, and replacing a signature is messier than not making it.

app_dir="$root/target/bundle/production"
"$root/scripts/bundle.sh" --channel production --release --no-sign >/dev/null
app="$app_dir/Tupli.app"
[ -d "$app" ] || die "bundle.sh did not produce $app"

# The universal binary over the single-architecture one bundle.sh just built.
cp "$fat" "$app/Contents/MacOS/tupli"

# Read back rather than repeated here: bundle.sh owns the channel contract, and
# a second copy of the identifier is a second thing to forget to change.
identifier=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' \
  "$app/Contents/Info.plist")

# ------------------------------------------------------------- the signature
#
# --options runtime is the hardened runtime, which notarization requires and
# which nothing here needs an exemption from: no JIT, no unsigned plug-ins, no
# loading anybody else's code. --timestamp asks Apple's timestamp server to
# countersign, so the signature stays valid after the certificate behind it
# expires. Both are the difference between this and what bundle.sh does.

say "signing"
codesign --force --options runtime --timestamp \
  --sign "$identity" --identifier "$identifier" "$app"
codesign --verify --deep --strict --verbose=2 "$app" 2>&1 | sed 's/^/release:   /'

# ----------------------------------------------------------------- notarize
#
# Twice, and in this order, for one reason: a ticket is stapled to a file, and
# the file it names has to be the file that ships.
#
# Notarizing only the image would leave the app inside it with no ticket of its
# own. That works while the image is the thing being opened — but the first
# thing anybody does is drag the app out of it, and from then on the copy in
# /Applications carries no proof. Gatekeeper falls back to asking Apple over
# the network, so a first launch offline is a first launch that fails.
#
# The app cannot be stapled before it is notarized, and the image cannot be
# built before the app is stapled without going stale the moment it is — so the
# app is submitted on its own first, stapled, and only then wrapped.

submit() {
  xcrun notarytool submit "$1" --keychain-profile "$profile" --wait \
    || die "notarization failed. For the reasons:
          xcrun notarytool log <submission-id> --keychain-profile $profile"
}

if [ "$notarize" = 1 ]; then
  # A zip only to have something to upload: notarytool takes an archive, not a
  # directory, and ditto is the one that preserves a bundle's symlinks and
  # extended attributes intact.
  zip="$root/target/release-dist/Tupli-$version.zip"
  rm -f "$zip"
  ditto -c -k --keepParent "$app" "$zip"
  say "notarizing the app — this takes a few minutes"
  submit "$zip"
  xcrun stapler staple "$app" >/dev/null
  rm -f "$zip"
fi

# -------------------------------------------------------------------- image
#
# A disk image rather than a zip, for the symlink: the window that opens has
# the app on one side and Applications on the other, and the gesture is obvious
# without a README.

dmg="$root/target/release-dist/Tupli-$version.dmg"
stage="$root/target/release-dist/stage"
rm -rf "$stage" "$dmg"
mkdir -p "$stage"
cp -R "$app" "$stage/"
ln -s /Applications "$stage/Applications"

say "building $(basename "$dmg")"
hdiutil create -volname "Tupli $version" -srcfolder "$stage" \
  -ov -format UDZO -quiet "$dmg"
rm -rf "$stage"

# Signed as well as the app inside it: an unsigned image is one more thing
# Gatekeeper has no opinion about.
codesign --force --timestamp --sign "$identity" "$dmg"

if [ "$notarize" = 1 ]; then
  say "notarizing the image"
  submit "$dmg"
  xcrun stapler staple "$dmg" >/dev/null
else
  say "note: not notarized. This image will not open on anybody else's Mac."
fi

# ------------------------------------------------------------------- verify
#
# Asked of the tools that will be asked on the other machine, rather than
# inferred from the fact that nothing errored on this one.

if [ "$notarize" = 1 ]; then
  for item in "$dmg" "$app"; do
    xcrun stapler validate "$item" | tail -1 | sed "s|^|release:   $(basename "$item"): |"
  done
  spctl -a -vvv -t open --context context:primary-signature "$dmg" 2>&1 \
    | sed 's/^/release:   /'
fi

say "$dmg"
say "$(du -h "$dmg" | cut -f1)"

# ----------------------------------------------------------------- publish
#
# Off by default and separate on purpose: everything above is reversible and
# this is not. A prerelease because the README still says alpha, and the two
# should not disagree.

if [ "$publish" = 1 ]; then
  tag="v$version"
  say "publishing $tag to $(git -C "$root" remote get-url origin)"
  gh release create "$tag" "$dmg" \
    --repo "$(gh repo view --json nameWithOwner -q .nameWithOwner)" \
    --title "Tupli $version" --prerelease --generate-notes
fi
