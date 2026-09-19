#!/usr/bin/env bash
set -euo pipefail
app="$1"
mode="${2:-developer-id}"
identity="-"
options=(--timestamp=none)
if [[ "$mode" == developer-id ]]; then
  : "${DEVELOPER_ID:?Set your own Developer ID Application identity}"
  : "${NOTARY_PROFILE:?Set a notarytool keychain profile}"
  identity="$DEVELOPER_ID"
  options=(--timestamp --options runtime)
elif [[ "$mode" != adhoc ]]; then
  echo "Unknown signing mode" >&2; exit 1
fi
# Nested code must be signed before its containing bundle, without --deep signing.
while IFS= read -r -d '' file; do
  if file -b "$file" | grep -q 'Mach-O'; then
    codesign --force --sign "$identity" "${options[@]}" "$file"
  fi
done < <(find "$app/Contents" -type f -print0)
while IFS= read -r -d '' bundle; do
  codesign --force --sign "$identity" "${options[@]}" "$bundle"
done < <(find "$app/Contents" -depth -type d \( -name '*.framework' -o -name '*.app' -o -name '*.xpc' -o -name '*.docktileplugin' \) -print0)
codesign --force --sign "$identity" "${options[@]}" "$app"
codesign --verify --deep --strict --verbose=2 "$app"
if [[ "$mode" == developer-id ]]; then
  zip_path="$(mktemp -d)/notarization.zip"
  trap 'rm -f "$zip_path"; rmdir "$(dirname "$zip_path")"' EXIT
  ditto -c -k --keepParent "$app" "$zip_path"
  xcrun notarytool submit "$zip_path" --keychain-profile "$NOTARY_PROFILE" --wait
  xcrun stapler staple "$app"
  xcrun stapler validate "$app"
  spctl --assess --type execute --verbose=2 "$app"
fi
