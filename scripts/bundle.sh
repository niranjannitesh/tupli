#!/bin/bash
#
# Wrap the tupli binary in a macOS .app bundle.
#
# The repository builds a plain executable. Finder, the Dock, Spotlight and the
# application switcher only show the ribbon icon once that executable is inside
# a bundle whose Info.plist names an icon resource, so this script is what turns
# `cargo build` output into something with a face.
#
# Channels are separate applications, not one application wearing three icons:
# Launch Services keys on the bundle identifier, so two builds sharing an
# identifier are the same app to macOS no matter what their icons look like, and
# installing one replaces the other. Each channel therefore gets its own name,
# its own identifier and its own icon, all three together.
#
# Usage:
#   scripts/bundle.sh [--channel development|preview|production] [--debug]
#                     [--no-sign] [--open]
#
# Environment:
#   TUPLI_CHANNEL        same as --channel; the flag wins
#   TUPLI_BUNDLE_PREFIX  reverse-DNS namespace (default: com.anuvaya)
#   TUPLI_APP_NAME       override the channel's display name
#   TUPLI_SIGN_IDENTITY  code-signing identity (default: Tupli Development,
#                        created by scripts/dev-identity.sh; falls back to ad hoc)

set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
channel=${TUPLI_CHANNEL:-production}
profile=release
sign=1
open_after=0

while [ $# -gt 0 ]; do
  case $1 in
    --channel) channel=${2:?--channel needs a value}; shift 2 ;;
    --channel=*) channel=${1#*=}; shift ;;
    --debug) profile=debug; shift ;;
    --release) profile=release; shift ;;
    --no-sign) sign=0; shift ;;
    --open) open_after=1; shift ;;
    -h|--help) sed -n '2,26p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "bundle: unknown argument: $1" >&2; exit 2 ;;
  esac
done

# The channel contract, kept in one place so the icon, the name and the
# identifier can never drift apart: changing only the icon is not enough for
# side-by-side installs.
case $channel in
  development) app_name="Tupli Dev";     id_suffix=".dev" ;;
  preview)     app_name="Tupli Preview"; id_suffix=".preview" ;;
  production)  app_name="Tupli";         id_suffix="" ;;
  master)
    echo "bundle: 'master' is the brand render, not an installable channel." >&2
    echo "        Use development, preview or production." >&2
    exit 2 ;;
  *)
    echo "bundle: unknown channel: $channel" >&2
    echo "        Expected development, preview or production." >&2
    exit 2 ;;
esac
app_name=${TUPLI_APP_NAME:-$app_name}

prefix=${TUPLI_BUNDLE_PREFIX:-com.anuvaya}
identifier="$prefix.tupli$id_suffix"

icon_src="$root/assets/app-icon-ribbon/$channel.icns"
[ -f "$icon_src" ] || { echo "bundle: missing icon: $icon_src" >&2; exit 1; }

# The one version number, read from the workspace rather than repeated here.
version=$(sed -n '/^\[workspace.package\]/,/^\[/p' "$root/Cargo.toml" \
  | sed -n 's/^version *= *"\(.*\)"/\1/p' | head -1)
[ -n "$version" ] || { echo "bundle: could not read the workspace version" >&2; exit 1; }

echo "bundle: $app_name $version ($channel, $profile) -> $identifier"

build_args=(build --bin tupli -p tupli)
[ "$profile" = release ] && build_args+=(--release)
(cd "$root" && cargo "${build_args[@]}")

binary="$root/target/$profile/tupli"
[ -x "$binary" ] || { echo "bundle: no binary at $binary" >&2; exit 1; }

# Staged under target/, which is already ignored: a bundle is build output and
# nothing in it is worth reviewing in a diff.
out="$root/target/bundle/$channel"
app="$out/$app_name.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"

cp "$binary" "$app/Contents/MacOS/tupli"
# A stable destination filename, so the plist never has to know which channel
# it is: the file that was copied here is the channel.
cp "$icon_src" "$app/Contents/Resources/tupli.icns"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleDevelopmentRegion</key>
	<string>en</string>
	<key>CFBundleDisplayName</key>
	<string>$app_name</string>
	<key>CFBundleExecutable</key>
	<string>tupli</string>
	<key>CFBundleIconFile</key>
	<string>tupli.icns</string>
	<key>CFBundleIdentifier</key>
	<string>$identifier</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleName</key>
	<string>$app_name</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>$version</string>
	<key>CFBundleVersion</key>
	<string>$version</string>
	<key>LSApplicationCategoryType</key>
	<string>public.app-category.developer-tools</string>
	<key>LSMinimumSystemVersion</key>
	<string>11.0</string>
	<key>NSHighResolutionCapable</key>
	<true/>
	<key>NSSupportsAutomaticGraphicsSwitching</key>
	<true/>
</dict>
</plist>
PLIST

printf 'APPL????' > "$app/Contents/PkgInfo"

if [ "$sign" = 1 ]; then
  # Not distribution signing: it is what lets a locally built bundle launch
  # without Gatekeeper arguing, and what gives the Keychain a code identity to
  # hang an item's ACL on. A real release is signed with a Developer ID and
  # notarized, after this script has finished.
  #
  # The local certificate is preferred over ad-hoc because an ad-hoc signature
  # is a hash of the binary: it changes with every build, so the Keychain sees
  # a new application each time and asks again for permission it was already
  # given. `scripts/dev-identity.sh` creates the certificate; without it this
  # falls back rather than failing, since signing at all matters more than
  # signing stably.
  identity=${TUPLI_SIGN_IDENTITY:-Tupli Development}
  if ! security find-identity -v -p codesigning 2>/dev/null | grep -qF "$identity"; then
    identity="-"
    [ "$channel" = production ] || echo "bundle: note: no local signing identity;" \
      "signing ad hoc. Run scripts/dev-identity.sh to stop the Keychain prompts." >&2
  fi
  # `--identifier` so the signature claims the bundle id even when signing ad
  # hoc, which is what a Keychain ACL is written against.
  codesign --force --sign "$identity" --identifier "$identifier" \
    --timestamp=none "$app" >/dev/null 2>&1 \
    || echo "bundle: warning: signing failed; the bundle is unsigned" >&2
fi

# Verify what was actually produced rather than what was intended.
plutil -lint "$app/Contents/Info.plist" >/dev/null
[ -f "$app/Contents/Resources/tupli.icns" ] || { echo "bundle: icon missing from the bundle" >&2; exit 1; }
file "$app/Contents/Resources/tupli.icns" | grep -q 'Mac OS X icon' \
  || { echo "bundle: Resources/tupli.icns is not an icon file" >&2; exit 1; }
[ "$sign" = 0 ] || codesign --verify --strict "$app" 2>/dev/null \
  || echo "bundle: warning: signature did not verify" >&2

echo "bundle: $app"
[ "$open_after" = 1 ] && open "$app"
exit 0
