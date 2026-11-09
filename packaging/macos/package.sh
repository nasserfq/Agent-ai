#!/usr/bin/env bash
# Build, sign and (when the Apple secrets are set) notarize Agent AI for macOS:
#
#   $DIST/agent-ai-<version>-macos-universal.dmg     Agent AI.app on a drag-to-Applications DMG
#
# One universal app: the Intel half runs on macOS 10.12 Sierra and later (a 2015 MacBook Pro
# included), the Apple silicon half on macOS 11 and later.
#
# Usage: packaging/macos/package.sh            (from the repository root; needs Xcode tools)
#
# Env:
#   VERSION               version to stamp (default: [workspace.package] version in Cargo.toml)
#   DIST                  output folder (default: dist)
#   MACOS_SIGN_IDENTITY   codesign identity (name or SHA-1). Default "-" = ad-hoc, for local tests
#                         only (Gatekeeper rejects it on other Macs). In CI, import-cert.sh sets it.
#   MACOS_KEYCHAIN        keychain holding the identity (optional)
#   APPLE_ID, APPLE_PASSWORD (app-specific password), APPLE_TEAM_ID: notarization (all three)
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
HERE="$ROOT/packaging/macos"
cd "$ROOT"
warn() { echo "warning: $*" >&2; }

VERSION="${VERSION:-$(awk '/^\[/ { p = ($0 == "[workspace.package]"); next } p && $1 == "version" { gsub(/[" ]/, "", $3); print $3; exit }' Cargo.toml)}"
DIST="${DIST:-$ROOT/dist}"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
IDENTITY="${MACOS_SIGN_IDENTITY:--}"
WORK="$TARGET_DIR/macos-package"
APP="$WORK/Agent AI.app"
DMG="$DIST/agent-ai-$VERSION-macos-universal.dmg"
mkdir -p "$DIST"

NOTARIZE=0
if [[ "$IDENTITY" = "-" ]]; then
  warn "ad-hoc signing (no MACOS_SIGN_IDENTITY); the app is not notarized"
elif [[ -n "${APPLE_ID:-}" && -n "${APPLE_PASSWORD:-}" && -n "${APPLE_TEAM_ID:-}" ]]; then
  NOTARIZE=1
else
  warn "APPLE_ID / APPLE_PASSWORD / APPLE_TEAM_ID incomplete; signed but not notarized"
fi
echo "==> Agent AI $VERSION for macOS, identity: $IDENTITY, notarize: $NOTARIZE"

# ---- build -------------------------------------------------------------------------------------
# The Intel half keeps LSMinimumSystemVersion in Info.plist.in (10.12); Apple silicon Macs all
# shipped with macOS 11 or later. Two builds, so C code in dependencies gets the right minimum too.
MACOSX_DEPLOYMENT_TARGET=10.12 cargo build --release --locked -p agent-ai --target x86_64-apple-darwin
MACOSX_DEPLOYMENT_TARGET=11.0 cargo build --release --locked -p agent-ai --target aarch64-apple-darwin

rm -rf "$WORK"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
lipo -create -output "$APP/Contents/MacOS/Agent AI" \
  "$TARGET_DIR/x86_64-apple-darwin/release/agent-ai" "$TARGET_DIR/aarch64-apple-darwin/release/agent-ai"
lipo -info "$APP/Contents/MacOS/Agent AI"
# The Intel half must really allow 10.12 (minos 10.12), or old Macs refuse to open the app.
otool -l -arch x86_64 "$APP/Contents/MacOS/Agent AI" | grep -A4 -E 'LC_BUILD_VERSION|LC_VERSION_MIN_MACOSX' | grep -E 'minos|version' | head -2

cp "$ROOT/assets/icon/agent-ai.icns" "$APP/Contents/Resources/AgentAI.icns"
cp "$ROOT/LICENSE-MIT" "$ROOT/LICENSE-APACHE" "$ROOT/NOTICE" "$ROOT/ATTRIBUTION.md" "$ROOT"/assets/fonts/OFL-*.txt "$APP/Contents/Resources/"
sed -e "s/@VERSION@/$VERSION/g" "$HERE/Info.plist.in" >"$APP/Contents/Info.plist"
plutil -lint "$APP/Contents/Info.plist"
printf 'APPL????' >"$APP/Contents/PkgInfo"

# ---- sign and notarize -------------------------------------------------------------------------
sign() {
  local ts=(--timestamp) kc=()
  [[ "$IDENTITY" = "-" ]] && ts=(--timestamp=none)
  [[ -n "${MACOS_KEYCHAIN:-}" ]] && kc=(--keychain "$MACOS_KEYCHAIN")
  codesign --force --sign "$IDENTITY" ${kc[@]+"${kc[@]}"} "${ts[@]}" "$@"
}

# The stapler fetches the ticket from Apple and sometimes loses the connection although
# notarization worked; both verbs are safe to repeat.
stapler_retry() {
  local try
  for try in 1 2 3 4; do
    xcrun stapler "$1" "$2" && return 0
    [[ "$try" = 4 ]] && break
    echo "stapler $1 failed (try $try of 4); retrying in $((try * 20)) s" >&2
    sleep $((try * 20))
  done
  echo "error: stapler $1 $(basename "$2") failed 4 times" >&2
  exit 1
}

notarize() {
  local file="$1" out id status
  echo "==> notarizing $(basename "$file") (this can take a few minutes)"
  out="$(xcrun notarytool submit "$file" --apple-id "$APPLE_ID" --password "$APPLE_PASSWORD" \
    --team-id "$APPLE_TEAM_ID" --wait --timeout 1h --output-format json)" || true
  echo "$out"
  id="$(printf '%s' "$out" | plutil -extract id raw -o - - 2>/dev/null || true)"
  status="$(printf '%s' "$out" | plutil -extract status raw -o - - 2>/dev/null || true)"
  if [[ "$status" != "Accepted" ]]; then
    [[ -n "$id" ]] && xcrun notarytool log "$id" --apple-id "$APPLE_ID" --password "$APPLE_PASSWORD" --team-id "$APPLE_TEAM_ID" || true
    echo "error: notarization of $(basename "$file") failed (status: ${status:-unknown})" >&2
    exit 1
  fi
}

# codesign --strict rejects extended attributes a copied file can carry; clear them first.
xattr -cr "$APP"
sign --options runtime --entitlements "$HERE/entitlements.plist" "$APP/Contents/MacOS/Agent AI"
sign --options runtime --entitlements "$HERE/entitlements.plist" "$APP"
codesign --verify --strict --deep --verbose=2 "$APP"

if [[ "$NOTARIZE" = 1 ]]; then
  ditto -c -k --keepParent "$APP" "$WORK/AgentAI-notarize.zip"
  notarize "$WORK/AgentAI-notarize.zip"
  stapler_retry staple "$APP"
  stapler_retry validate "$APP"
  spctl --assess --type execute -vvv "$APP"
fi

# ---- DMG ---------------------------------------------------------------------------------------
echo "==> building $DMG"
STAGE="$WORK/dmg"
mkdir -p "$STAGE"
ditto "$APP" "$STAGE/Agent AI.app"
ln -s /Applications "$STAGE/Applications"
rm -f "$DMG"
# hdiutil now and then fails on CI runners with "Resource busy"; try again with growing pauses.
for attempt in 1 2 3 4 5; do
  if hdiutil create -srcfolder "$STAGE" -volname "Agent AI" -fs HFS+ -format UDZO -imagekey zlib-level=9 -ov "$DMG"; then
    break
  fi
  [[ "$attempt" = 5 ]] && { echo "error: hdiutil create failed 5 times" >&2; exit 1; }
  warn "hdiutil create failed (attempt $attempt of 5); retrying in $((attempt * 10)) s"
  sleep $((attempt * 10))
done
sign "$DMG"
codesign --verify --strict --verbose=2 "$DMG"
if [[ "$NOTARIZE" = 1 ]]; then
  notarize "$DMG"
  stapler_retry staple "$DMG"
  stapler_retry validate "$DMG"
fi
echo "==> done"
ls -lh "$DMG"
