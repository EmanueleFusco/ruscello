%global debug_package %{nil}

Name:           ruscello
Version:        %{pkg_version}
Release:        1%{?dist}
Summary:        Desktop download and BitTorrent manager

License:        GPL-3.0-or-later
URL:            https://github.com/EmanueleFusco/ruscello

Requires:       hicolor-icon-theme
Requires:       libxkbcommon

%description
Ruscello Download Manager is a desktop HTTP/FTP, media, and BitTorrent
download manager written in Rust.

%install
install -Dm755 "%{bin_dir}/ruscello" "%{buildroot}%{_bindir}/ruscello"
install -Dm755 "%{bin_dir}/ruscello-webview" "%{buildroot}%{_bindir}/ruscello-webview"
install -Dm644 "%{src_dir}/packaging/linux/org.ruscello.Ruscello.desktop" \
  "%{buildroot}%{_datadir}/applications/org.ruscello.Ruscello.desktop"
install -Dm644 "%{src_dir}/apps/gui/assets/ruscello.png" \
  "%{buildroot}%{_datadir}/icons/hicolor/512x512/apps/ruscello.png"
install -Dm644 "%{src_dir}/LICENSE" \
  "%{buildroot}%{_datadir}/licenses/ruscello/LICENSE"

%files
%{_bindir}/ruscello
%{_bindir}/ruscello-webview
%{_datadir}/applications/org.ruscello.Ruscello.desktop
%{_datadir}/icons/hicolor/512x512/apps/ruscello.png
%{_datadir}/licenses/ruscello/LICENSE
