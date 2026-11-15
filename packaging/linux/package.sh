#!/usr/bin/env bash
set -euo pipefail

tag=${1:?Pass the release tag}
bin=${2:-target/release}
out=${3:-dist}
appimagetool=${APPIMAGETOOL:?Set APPIMAGETOOL to the AppImage packaging tool}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$out"
out=$(realpath "$out")
app="$work/Ruscello.AppDir"
desktop_file=org.ruscello.Ruscello.desktop
install -Dm755 "$bin/ruscello" "$app/usr/bin/ruscello"
install -Dm755 "$bin/ruscello-webview" "$app/usr/bin/ruscello-webview"
install -Dm644 "packaging/linux/$desktop_file" "$app/usr/share/applications/$desktop_file"
icon=usr/share/icons/hicolor/512x512/apps/ruscello.png
install -Dm644 apps/gui/assets/ruscello.png "$app/$icon"
install -Dm644 LICENSE "$app/usr/share/doc/ruscello/copyright"
install -Dm755 packaging/linux/AppRun "$app/AppRun"
ln -s "usr/share/applications/$desktop_file" "$app/$desktop_file"
ln -s "$icon" "$app/ruscello.png"
ln -s ruscello.png "$app/.DirIcon"
stem="ruscello-$tag-linux-x86_64"
ARCH=x86_64 VERSION="${tag#v}" APPIMAGE_EXTRACT_AND_RUN=1 \
  "$appimagetool" --no-appstream "$app" "$out/$stem.AppImage"
rm "$app/AppRun" "$app/$desktop_file" "$app/ruscello.png" "$app/.DirIcon"
mkdir "$app/DEBIAN"
mkdir "$work/debian"
printf 'Source: ruscello\n\nPackage: ruscello\nArchitecture: any\n' > "$work/debian/control"
depends=$(cd "$work"; dpkg-shlibdeps -O -e"$app/usr/bin/ruscello" \
  -e"$app/usr/bin/ruscello-webview" | sed -n 's/^shlibs:Depends=//p')
test -n "$depends"
cat > "$app/DEBIAN/control" <<CONTROL
Package: ruscello
Version: ${tag#v}
Architecture: amd64
Maintainer: Ruscello maintainers
Section: net
Priority: optional
Depends: $depends
Homepage: https://github.com/EmanueleFusco/ruscello
Description: Desktop download and BitTorrent manager
CONTROL
dpkg-deb --root-owner-group --build "$app" "$out/$stem.deb"
