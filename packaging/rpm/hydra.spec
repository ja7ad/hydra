%{!?_version: %global _version 0.3.8}

Name:           hydra
Version:        %{_version}
Release:        1%{?dist}
Summary:        Multi-connection download manager (GUI, CLI, browser integration)

License:        GPL-3.0-or-later
URL:            https://github.com/ja7ad/hydra
Source0:        https://github.com/ja7ad/hydra/archive/v%{version}/hydra-%{version}.tar.gz

Recommends:     gnome-shell-extension-appindicator

BuildRequires:  cargo
BuildRequires:  rust >= 1.75
BuildRequires:  pkgconfig
BuildRequires:  alsa-lib-devel
BuildRequires:  libX11-devel
BuildRequires:  libXrandr-devel
BuildRequires:  libxcb-devel
BuildRequires:  libxkbcommon-devel
BuildRequires:  python3
BuildRequires:  desktop-file-utils

%description
Hydra downloads files over many parallel connections with integrity
verification. This package installs the hydra CLI, the hydra-gui desktop
app (with menu entry and login autostart), the hydra-host
native-messaging bridge plus browser manifests for extension capture, and
the browser extensions under /usr/share/hydra/extensions.

%prep
%autosetup -n hydra-%{version}

%build
cargo build --release -p hya-cli -p hya-gui -p hya-host

%install
rm -rf %{buildroot}

install -Dm755 target/release/hydra      %{buildroot}%{_bindir}/hydra
install -Dm755 target/release/hydra-gui  %{buildroot}%{_bindir}/hydra-gui
install -Dm755 target/release/hydra-host %{buildroot}%{_bindir}/hydra-host

# Desktop / Menu
install -d %{buildroot}%{_datadir}/applications
cat > %{buildroot}%{_datadir}/applications/hydra.desktop << 'EOF'
[Desktop Entry]
Type=Application
Name=Hydra Download Manager
GenericName=Download Manager
Comment=Multi-connection download accelerator
Exec=hydra-gui
Icon=hydra
Terminal=false
Categories=Network;FileTransfer;
StartupWMClass=hydra
EOF

# Autostart
install -d %{buildroot}%{_sysconfdir}/xdg/autostart
cat > %{buildroot}%{_sysconfdir}/xdg/autostart/hydra.desktop << 'EOF'
[Desktop Entry]
Type=Application
Name=Hydra Download Manager
Comment=Multi-connection download accelerator
Exec=hydra-gui --minimized
Icon=hydra
Terminal=false
Categories=Network;FileTransfer;
X-GNOME-Autostart-enabled=true
EOF

# Icons
install -Dm644 docs/logo.png %{buildroot}%{_datadir}/icons/hicolor/512x512/apps/hydra.png

# Extensions
install -d %{buildroot}%{_datadir}/%{name}/extensions
scripts/build-extensions.sh --out "%{buildroot}%{_datadir}/%{name}/extensions" \
  --quiet --prefix "%{_datadir}/%{name}/extensions"

# Man pages
install -d %{buildroot}%{_mandir}/man1
for page in docs/man/*.1; do
  install -m644 "$page" %{buildroot}%{_mandir}/man1/
done

# Native messaging manifests
for d in etc/opt/chrome etc/chromium etc/opt/edge; do
  install -d %{buildroot}/$d/native-messaging-hosts
done
install -d %{buildroot}%{_prefix}/lib/mozilla/native-messaging-hosts
install -d %{buildroot}%{_prefix}/lib64/mozilla/native-messaging-hosts

cat > %{buildroot}%{_sysconfdir}/opt/chrome/native-messaging-hosts/com.hydra.host.json << 'EOF'
{
  "name": "com.hydra.host",
  "description": "Hydra Download Manager native host",
  "path": "/usr/bin/hydra-host",
  "type": "stdio",
  "allowed_origins": ["chrome-extension://jpnonmbbkjdpeebdhkjoliklfhkdcomj/"]
}
EOF
cp %{buildroot}%{_sysconfdir}/opt/chrome/native-messaging-hosts/com.hydra.host.json %{buildroot}%{_sysconfdir}/chromium/native-messaging-hosts/com.hydra.host.json
cp %{buildroot}%{_sysconfdir}/opt/chrome/native-messaging-hosts/com.hydra.host.json %{buildroot}%{_sysconfdir}/opt/edge/native-messaging-hosts/com.hydra.host.json

cat > %{buildroot}%{_prefix}/lib/mozilla/native-messaging-hosts/com.hydra.host.json << 'EOF'
{
  "name": "com.hydra.host",
  "description": "Hydra Download Manager native host",
  "path": "/usr/bin/hydra-host",
  "type": "stdio",
  "allowed_extensions": ["hydra@ja7ad.github.io"]
}
EOF
cp %{buildroot}%{_prefix}/lib/mozilla/native-messaging-hosts/com.hydra.host.json %{buildroot}%{_prefix}/lib64/mozilla/native-messaging-hosts/com.hydra.host.json

%post
if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database -q %{_datadir}/applications || :
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache -qt %{_datadir}/icons/hicolor || :
fi

%postun
if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database -q %{_datadir}/applications || :
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache -qt %{_datadir}/icons/hicolor || :
fi

%files
%license LICENSE LICENSING.md
%doc README.md THIRD-PARTY-NOTICES.md
%{_bindir}/hydra
%{_bindir}/hydra-gui
%{_bindir}/hydra-host
%{_datadir}/applications/hydra.desktop
%{_datadir}/icons/hicolor/*/apps/hydra.png
%{_datadir}/%{name}/
%{_mandir}/man1/hydra*.1*
%config(noreplace) %{_sysconfdir}/xdg/autostart/hydra.desktop
%config(noreplace) %{_sysconfdir}/opt/chrome/native-messaging-hosts/com.hydra.host.json
%config(noreplace) %{_sysconfdir}/chromium/native-messaging-hosts/com.hydra.host.json
%config(noreplace) %{_sysconfdir}/opt/edge/native-messaging-hosts/com.hydra.host.json
%{_prefix}/lib/mozilla/native-messaging-hosts/com.hydra.host.json
%{_prefix}/lib64/mozilla/native-messaging-hosts/com.hydra.host.json

%changelog
* Sat Aug 22 2026 Javad Rajabzadeh <ja7ad@live.com> - 0.3.8-1
- Initial RPM release
