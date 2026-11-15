#!/usr/bin/env bash
set -euo pipefail

tag=${1:?Pass the release tag}
bin=${2:-target/release}
out=${3:-dist}
host_arch=${TARGET_ARCH:-$(uname -m)}
case "$host_arch" in
  x86_64|amd64)
    arch=x86_64
    ;;
  aarch64|arm64)
    arch=aarch64
    ;;
  *)
    echo "Unsupported Linux RPM architecture: $host_arch" >&2
    exit 1
    ;;
esac

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$out" "$work/rpmbuild" "$work/rpms" "$work/buildroot"
out=$(realpath "$out")
bin=$(realpath "$bin")
src=$(realpath .)
desktop_file="packaging/linux/org.ruscello.Ruscello.desktop"
desktop-file-validate "$desktop_file"

rpmbuild -bb \
  --target "$arch" \
  --buildroot "$work/buildroot" \
  --define "_topdir $work/rpmbuild" \
  --define "_rpmdir $work/rpms" \
  --define "pkg_version ${tag#v}" \
  --define "bin_dir $bin" \
  --define "src_dir $src" \
  packaging/linux/ruscello.spec

rpm_file=$(find "$work/rpms" -type f -name '*.rpm' | head -n 1)
test -n "$rpm_file"
cp "$rpm_file" "$out/ruscello-$tag-linux-$arch.rpm"
