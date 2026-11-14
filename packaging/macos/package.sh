#!/usr/bin/env bash
set -euo pipefail

tag=${1:?Pass the release tag}
bin=${2:-target/release}
out=${3:-dist}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
app="$work/Ruscello.app"
mkdir -p "$out" "$app/Contents/Resources"
mkdir -p "$app/Contents/MacOS"
install -m755 "$bin/ruscello" "$app/Contents/MacOS/ruscello"
install -m755 "$bin/ruscello-webview" "$app/Contents/MacOS/ruscello-webview"
install -m644 LICENSE "$app/Contents/Resources/LICENSE"
sed "s/@VERSION@/${tag#v}/g" packaging/macos/Info.plist > "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist"
codesign --force --sign - "$app/Contents/MacOS/ruscello-webview"
codesign --force --sign - "$app"
codesign --verify --deep --strict "$app"
ln -s /Applications "$work/Applications"
arch=$(uname -m)
if [[ $arch == arm64 ]]; then arch=aarch64; fi
hdiutil create -fs HFS+ -volname Ruscello -srcfolder "$work" -format UDZO \
  "$out/ruscello-$tag-macos-$arch.dmg"
