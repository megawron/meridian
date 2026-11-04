#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Build release packages for Meridian locally or in CI.
#
# Usage:
#   ./scripts/build-packages.sh local [tag]     # builds Flatpak + DEB + tarball locally
#   ./scripts/build-packages.sh upload <tag>    # builds locally and uploads to GitHub release
#   ./scripts/build-packages.sh flatpak [tag]   # builds Flatpak bundle (.flatpak) locally
#   ./scripts/build-packages.sh rpm [tag]       # builds Fedora RPM (requires rpmbuild)
#   ./scripts/build-packages.sh deb [tag]       # builds Debian/Ubuntu DEB

set -euo pipefail

APP_ID="com.martingawron.Meridian"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$ROOT/packaging/out"
TAG="${2:-$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)}"
RAW_VERSION="${TAG#v}"
PKG_VERSION="${RAW_VERSION//-/\~}"
WHAT="${1:-local}"

[ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env"
mkdir -p "$OUT"

ensure_binary() {
    echo "==> Building release binary"
    ( cd "$ROOT" && cargo build --release --locked --bin meridian )
}

stage_common() {
    local prefix="$1"
    install -Dm755 "$ROOT/target/release/meridian" "$prefix/bin/meridian"
    install -Dm644 "$ROOT/data/$APP_ID.gschema.xml" "$prefix/share/glib-2.0/schemas/$APP_ID.gschema.xml"
    glib-compile-schemas "$prefix/share/glib-2.0/schemas" 2>/dev/null || true
    install -Dm644 "$ROOT/data/$APP_ID.desktop.in" "$prefix/share/applications/$APP_ID.desktop"
    install -Dm644 "$ROOT/data/$APP_ID.Calendar.desktop.in" "$prefix/share/applications/$APP_ID.Calendar.desktop"
    install -Dm644 "$ROOT/data/$APP_ID.metainfo.xml.in" "$prefix/share/metainfo/$APP_ID.metainfo.xml"

    for size in 16 22 24 32 48 64 128 256 512; do
        if [ -f "$ROOT/data/icons/hicolor/${size}x${size}/apps/$APP_ID.png" ]; then
            install -Dm644 "$ROOT/data/icons/hicolor/${size}x${size}/apps/$APP_ID.png" \
                "$prefix/share/icons/hicolor/${size}x${size}/apps/$APP_ID.png"
        fi
    done

    for svg in "$ROOT"/data/icons/hicolor/scalable/actions/*.svg; do
        [ -f "$svg" ] || continue
        install -Dm644 "$svg" "$prefix/share/icons/hicolor/scalable/actions/$(basename "$svg")"
    done

    for svg in "$ROOT"/data/icons/hicolor/scalable/apps/*.svg; do
        [ -f "$svg" ] || continue
        install -Dm644 "$svg" "$prefix/share/icons/hicolor/scalable/apps/$(basename "$svg")"
    done

    for po in "$ROOT"/po/*.po; do
        [ -f "$po" ] || continue
        lang="$(basename "$po" .po)"
        install -d "$prefix/share/locale/$lang/LC_MESSAGES"
        msgfmt "$po" -o "$prefix/share/locale/$lang/LC_MESSAGES/meridian.mo"
    done
}

build_flatpak_bundle() {
    ensure_binary
    local arch bundle work build_dir repo_dir
    arch="$(uname -m)"
    case "$TAG" in
        *-beta*) bundle="Meridian-Beta-${arch}.flatpak" ;;
        *)       bundle="Meridian-${arch}.flatpak" ;;
    esac

    echo "==> Packaging Flatpak bundle: $bundle"
    work="$(mktemp -d)"
    build_dir="$work/build-dir"
    repo_dir="$work/repo"

    mkdir -p "$build_dir/files/lib" "$build_dir/var" \
        "$build_dir/files/share/app-info/xmls" \
        "$build_dir/files/share/app-info/icons/flatpak/64x64" \
        "$build_dir/files/share/app-info/icons/flatpak/128x128"
    cat > "$build_dir/metadata" <<EOF
[Application]
name=$APP_ID
runtime=org.gnome.Platform/$arch/48
sdk=org.gnome.Sdk/$arch/48
EOF
    stage_common "$build_dir/files"

    cp "$ROOT/data/icons/hicolor/64x64/apps/$APP_ID.png" \
        "$build_dir/files/share/app-info/icons/flatpak/64x64/$APP_ID.png"
    cp "$ROOT/data/icons/hicolor/128x128/apps/$APP_ID.png" \
        "$build_dir/files/share/app-info/icons/flatpak/128x128/$APP_ID.png"

    cat <<EOF | gzip -c > "$build_dir/files/share/app-info/xmls/$APP_ID.xml.gz"
<?xml version="1.0" encoding="UTF-8"?>
<components version="0.8" origin="flatpak">
  <component type="desktop-application">
    <id>$APP_ID</id>
    <name>Meridian</name>
    <summary>Mail, calendar, contacts, and tasks for GNOME</summary>
    <project_license>GPL-3.0-or-later</project_license>
    <developer id="com.martingawron">
      <name>Martin Gawron</name>
    </developer>
    <description>
      <p>Meridian is an all-in-one mail, calendar, contacts, and tasks application designed for the GNOME desktop, built with Rust, GTK 4, libadwaita, and WebKitGTK.</p>
      <p>Built on top of Evolution Data Server (EDS), Meridian works out of the box with your existing system accounts, local folders, and GNOME Online Accounts.</p>
    </description>
    <icon type="cached" height="128" width="128">$APP_ID.png</icon>
    <icon type="cached" height="64" width="64">$APP_ID.png</icon>
    <launchable type="desktop-id">$APP_ID.desktop</launchable>
    <url type="homepage">https://github.com/megawron/meridian</url>
    <url type="bugtracker">https://github.com/megawron/meridian/issues</url>
    <url type="vcs-browser">https://github.com/megawron/meridian</url>
    <content_rating type="oars-1.1"/>
    <releases>
      <release version="$RAW_VERSION" date="$(date +%Y-%m-%d)">
        <description>
          <p>Public beta release of Meridian featuring integrated mail, calendar, contacts, tasks, and memos.</p>
        </description>
      </release>
    </releases>
  </component>
</components>
EOF

    ldd "$ROOT/target/release/meridian" | awk '/=> \// { print $3 }' | while read -r lib; do
        base="$(basename "$lib")"
        case "$base" in
            libcamel*|libedataserver*|libecal*|libebook*|libebackend*|libical*|libgoa*|libphonenumber*|libprotobuf*)
                cp -L "$lib" "$build_dir/files/lib/$base"
                ;;
        esac
    done

    flatpak build-finish "$build_dir" \
        --command=meridian \
        --share=ipc \
        --share=network \
        --socket=fallback-x11 \
        --socket=wayland \
        --device=dri \
        --talk-name=org.gnome.evolution.dataserver.Sources5 \
        --talk-name=org.gnome.evolution.dataserver.Calendar8 \
        --talk-name=org.gnome.evolution.dataserver.AddressBook10 \
        --talk-name=org.gnome.evolution.dataserver.Subprocess.Backend.* \
        --talk-name=org.gnome.OnlineAccounts \
        --talk-name=org.freedesktop.Notifications \
        --talk-name=org.freedesktop.secrets \
        --filesystem=xdg-run/gvfsd \
        --metadata=X-DConf=migrate-path=/com/martingawron/meridian/ >/dev/null

    flatpak build-export "$repo_dir" "$build_dir" >/dev/null
    flatpak build-bundle "$repo_dir" "$OUT/$bundle" "$APP_ID" \
        --runtime-repo=https://flathub.org/repo/flathub.flatpakrepo
    rm -rf "$work"
    echo "==> Flatpak ready: $OUT/$bundle"
}

build_deb() {
    ensure_binary
    local work pkg arch deb_file
    work="$(mktemp -d)"
    pkg="$work/data"
    arch="amd64"
    [ "$(uname -m)" = "aarch64" ] && arch="arm64"
    deb_file="$OUT/meridian_${PKG_VERSION}-1_${arch}.deb"

    echo "==> Packaging Debian/Ubuntu package: $(basename "$deb_file")"
    mkdir -p "$pkg/usr" "$pkg/usr/share/doc/meridian" "$work/control"
    stage_common "$pkg/usr"
    rm -f "$pkg/usr/share/glib-2.0/schemas/gschemas.compiled"
    cp "$ROOT/LICENSE" "$pkg/usr/share/doc/meridian/copyright"

    cat > "$work/control/control" <<EOF
Package: meridian
Version: ${PKG_VERSION}-1
Section: mail
Priority: optional
Architecture: $arch
Maintainer: Martin Gawron <martin@martingawron.com>
Installed-Size: $(du -sk "$pkg" | cut -f1)
Depends: evolution-data-server, libgtk-4-1 (>= 4.16), libadwaita-1-0 (>= 1.6), libwebkitgtk-6.0-4, libsqlite3-0, librsvg2-common | glycin-loaders
Recommends: gnome-keyring, gnome-online-accounts
Homepage: https://github.com/megawron/meridian
Description: GNOME-native mail, calendar, contacts, and tasks client
 Meridian is an all-in-one mail, calendar, contacts, and tasks application
 designed for the GNOME desktop, built with Rust, GTK 4, libadwaita, and
 WebKitGTK on top of Evolution Data Server (EDS).
EOF

    cat > "$work/control/postinst" <<'EOF'
#!/bin/sh
set -e
if [ "$1" = "configure" ] && command -v glib-compile-schemas >/dev/null 2>&1; then
    glib-compile-schemas /usr/share/glib-2.0/schemas || true
fi
EOF
    chmod 0755 "$work/control/postinst"

    if command -v dpkg-deb >/dev/null 2>&1; then
        mkdir -p "$pkg/DEBIAN"
        cp "$work/control/"* "$pkg/DEBIAN/"
        dpkg-deb --build --root-owner-group "$pkg" "$deb_file"
    else
        printf "2.0\n" > "$work/debian-binary"
        tar --owner=0 --group=0 -C "$work/control" -czf "$work/control.tar.gz" .
        tar --owner=0 --group=0 -C "$pkg" -czf "$work/data.tar.gz" .
        rm -f "$deb_file"
        ( cd "$work" && ar rcs "$deb_file" debian-binary control.tar.gz data.tar.gz )
    fi

    rm -rf "$work"
    echo "==> DEB ready: $deb_file"
}

build_tarball() {
    ensure_binary
    local arch tar_name work
    arch="$(uname -m)"
    tar_name="meridian-${RAW_VERSION}-linux-${arch}.tar.gz"
    work="$(mktemp -d)"
    mkdir -p "$work/meridian-${RAW_VERSION}/usr"
    stage_common "$work/meridian-${RAW_VERSION}/usr"
    cp "$ROOT/LICENSE" "$ROOT/README.md" "$work/meridian-${RAW_VERSION}/"
    tar --owner=0 --group=0 -C "$work" -czf "$OUT/$tar_name" "meridian-${RAW_VERSION}"
    rm -rf "$work"
    echo "==> Tarball ready: $OUT/$tar_name"
}

build_rpm() {
    ensure_binary
    local work stage arch
    work="$(mktemp -d)"
    stage="$work/meridian-$PKG_VERSION-bin"
    mkdir -p "$stage/usr" "$work/rpmbuild/SOURCES" "$work/rpmbuild/SPECS"
    stage_common "$stage/usr"
    cp "$ROOT/LICENSE" "$stage/LICENSE"
    rm -f "$stage/usr/share/glib-2.0/schemas/gschemas.compiled"

    tar -C "$work" -cf "$work/rpmbuild/SOURCES/meridian-$PKG_VERSION-bin.tar" "meridian-$PKG_VERSION-bin"

    cat > "$work/rpmbuild/SPECS/meridian.spec" <<EOF
%global debug_package %{nil}

Name:           meridian
Version:        $PKG_VERSION
Release:        1%{?dist}
Summary:        A GNOME-native mail, calendar, contacts, and tasks client
License:        GPL-3.0-or-later
URL:            https://github.com/megawron/meridian
Source0:        %{name}-%{version}-bin.tar
Requires:       evolution-data-server
Recommends:     gnome-keyring

%description
Meridian is an all-in-one mail, calendar, contacts, and tasks application
designed for the GNOME desktop, built with Rust, GTK 4, libadwaita, and
WebKitGTK on top of Evolution Data Server (EDS).

%prep
%setup -q -n %{name}-%{version}-bin

%install
cp -a usr %{buildroot}/usr

%files
%license LICENSE
%{_bindir}/meridian
%{_datadir}/applications/$APP_ID.desktop
%{_datadir}/applications/$APP_ID.Calendar.desktop
%{_datadir}/metainfo/$APP_ID.metainfo.xml
%{_datadir}/glib-2.0/schemas/$APP_ID.gschema.xml
%{_datadir}/icons/hicolor/*/apps/$APP_ID.png
%{_datadir}/icons/hicolor/scalable/actions/*.svg
%{_datadir}/icons/hicolor/scalable/apps/*.svg
%{_datadir}/locale/*/LC_MESSAGES/meridian.mo
EOF

    rpmbuild -bb --define "_topdir $work/rpmbuild" "$work/rpmbuild/SPECS/meridian.spec"
    arch="$(rpm --eval '%{_arch}')"
    cp "$work/rpmbuild/RPMS/$arch/"meridian-*.rpm "$OUT/"
    rm -rf "$work"
    echo "==> RPM built in $OUT"
}

stage_flatpak() {
    ensure_binary
    local fp_stage="$OUT/flatpak-app"
    rm -rf "$fp_stage"
    mkdir -p "$fp_stage/lib"
    stage_common "$fp_stage"

    ldd "$ROOT/target/release/meridian" | awk '/=> \// { print $3 }' | while read -r lib; do
        base="$(basename "$lib")"
        case "$base" in
            libcamel*|libedataserver*|libecal*|libebook*|libebackend*|libical*|libgoa*|libphonenumber*|libprotobuf*)
                cp -L "$lib" "$fp_stage/lib/$base"
                ;;
        esac
    done
    tar -C "$OUT" -czf "$OUT/flatpak-app.tar.gz" flatpak-app
    rm -rf "$fp_stage"
}

build_local_all() {
    rm -f "$OUT"/*
    build_flatpak_bundle
    build_deb
    build_tarball
    if command -v rpmbuild >/dev/null 2>&1; then
        build_rpm
    fi
    ( cd "$OUT" && sha256sum * > SHA256SUMS.txt )
    echo "==> All local release packages built in $OUT:"
    ls -lh "$OUT"
}

case "$WHAT" in
    local) build_local_all ;;
    upload)
        build_local_all
        echo "==> Uploading packages to GitHub release $TAG..."
        gh release upload "$TAG" "$OUT"/* --clobber
        echo "==> Uploaded successfully to $TAG!"
        ;;
    flatpak) build_flatpak_bundle ;;
    rpm) build_rpm ;;
    deb) build_deb ;;
    flatpak-stage) stage_flatpak ;;
    deb-and-flatpak-stage) build_deb; stage_flatpak ;;
    *) echo "Usage: $0 {local|upload|flatpak|rpm|deb} [tag]" >&2; exit 1 ;;
esac
