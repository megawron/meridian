#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Build native release packages (.rpm, .deb) and Flatpak bundle for Meridian.
#
# Usage:
#   ./scripts/build-packages.sh rpm [version]
#   ./scripts/build-packages.sh deb [version]
#   ./scripts/build-packages.sh flatpak-stage [version]

set -euo pipefail

APP_ID="com.martingawron.Meridian"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$ROOT/packaging/out"
RAW_VERSION="${2:-$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)}"
RAW_VERSION="${RAW_VERSION#v}"
PKG_VERSION="${RAW_VERSION//-/\~}"
WHAT="${1:-}"

[ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env"
mkdir -p "$OUT"

stage_common() {
    local prefix="$1"
    install -Dm755 "$ROOT/target/release/meridian" "$prefix/bin/meridian"
    install -Dm644 "$ROOT/data/$APP_ID.gschema.xml" "$prefix/share/glib-2.0/schemas/$APP_ID.gschema.xml"
    glib-compile-schemas "$prefix/share/glib-2.0/schemas" || true
    install -Dm644 "$ROOT/data/$APP_ID.desktop.in" "$prefix/share/applications/$APP_ID.desktop"
    install -Dm644 "$ROOT/data/$APP_ID.Calendar.desktop.in" "$prefix/share/applications/$APP_ID.Calendar.desktop"
    install -Dm644 "$ROOT/data/$APP_ID.metainfo.xml.in" "$prefix/share/metainfo/$APP_ID.metainfo.xml"

    for size in 16 22 24 32 48 64 128 256 512 1024; do
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

build_rpm() {
    echo "==> Building release binary for RPM"
    ( cd "$ROOT" && cargo build --release --locked --bin meridian )

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

build_deb() {
    echo "==> Building release binary for DEB"
    ( cd "$ROOT" && cargo build --release --locked --bin meridian )

    local work pkg arch deps
    work="$(mktemp -d)"
    pkg="$work/meridian"
    arch="$(dpkg --print-architecture)"
    mkdir -p "$pkg/usr" "$pkg/DEBIAN" "$pkg/usr/share/doc/meridian"
    stage_common "$pkg/usr"
    rm -f "$pkg/usr/share/glib-2.0/schemas/gschemas.compiled"
    cp "$ROOT/LICENSE" "$pkg/usr/share/doc/meridian/copyright"

    mkdir -p "$work/debian"
    printf 'Source: meridian\n\nPackage: meridian\nArchitecture: any\n' > "$work/debian/control"
    deps="$(cd "$work" && dpkg-shlibdeps -O -e"$pkg/usr/bin/meridian" 2>/dev/null)"
    deps="${deps#shlibs:Depends=}"

    cat > "$pkg/DEBIAN/control" <<EOF
Package: meridian
Version: ${PKG_VERSION}-1
Section: mail
Priority: optional
Architecture: $arch
Maintainer: Martin Gawron <martin@martingawron.com>
Installed-Size: $(du -sk --exclude=DEBIAN "$pkg" | cut -f1)
Depends: $deps, evolution-data-server, librsvg2-common | glycin-loaders
Recommends: gnome-keyring, gnome-online-accounts
Homepage: https://github.com/megawron/meridian
Description: GNOME-native mail, calendar, contacts, and tasks client
 Meridian is an all-in-one mail, calendar, contacts, and tasks application
 designed for the GNOME desktop, built with Rust, GTK 4, libadwaita, and
 WebKitGTK on top of Evolution Data Server (EDS).
EOF

    cat > "$pkg/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e
if [ "$1" = "configure" ] && command -v glib-compile-schemas >/dev/null 2>&1; then
    glib-compile-schemas /usr/share/glib-2.0/schemas || true
fi
EOF
    chmod 0755 "$pkg/DEBIAN/postinst"

    dpkg-deb --build --root-owner-group "$pkg" "$OUT/meridian_${PKG_VERSION}-1_${arch}.deb"
    rm -rf "$work"
    echo "==> DEB built in $OUT"
}

stage_flatpak() {
    local fp_stage="$OUT/flatpak-app"
    rm -rf "$fp_stage"
    mkdir -p "$fp_stage/lib"
    stage_common "$fp_stage"

    # Bundle the EDS and companion shared libraries not included in org.gnome.Platform//48
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
    echo "==> Flatpak app payload staged at $OUT/flatpak-app.tar.gz"
}

case "$WHAT" in
    rpm) build_rpm ;;
    deb) build_deb ;;
    flatpak-stage) stage_flatpak ;;
    deb-and-flatpak-stage) build_deb; stage_flatpak ;;
    *) echo "Usage: $0 {rpm|deb|flatpak-stage|deb-and-flatpak-stage} [version]" >&2; exit 1 ;;
esac
