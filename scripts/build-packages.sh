#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Build release packages for Meridian locally or in CI.
#
# Usage:
#   ./scripts/build-packages.sh local [tag]     # builds Flatpak + RPM locally
#   ./scripts/build-packages.sh upload <tag>    # builds locally and uploads to GitHub release
#   ./scripts/build-packages.sh flatpak [tag]   # builds Flatpak bundle (.flatpak) locally
#   ./scripts/build-packages.sh rpm [tag]       # builds Fedora RPM (requires rpmbuild)
#   ./scripts/build-packages.sh merge-repos <dest> <src>...  # merges per-arch Flatpak repos into <dest>

set -euo pipefail

APP_ID="com.martingawron.Meridian"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$ROOT/packaging/out"
DEFAULT_VER="$(sed -n 's/.*<release version="\([^"]*\)".*/\1/p' "$ROOT/data/$APP_ID.metainfo.xml.in" | head -1)"
[ -z "$DEFAULT_VER" ] && DEFAULT_VER="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)"
TAG="${2:-$DEFAULT_VER}"
RAW_VERSION="${TAG#v}"
PKG_VERSION="${RAW_VERSION//-/\~}"
WHAT="${1:-local}"

[ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env"
mkdir -p "$OUT"

ensure_binary() {
    if [ -x "$ROOT/target/release/meridian" ] && [ "${FORCE_REBUILD:-0}" != "1" ]; then
        echo "==> Using existing release binary: $ROOT/target/release/meridian"
        return 0
    fi
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

    for svg in "$ROOT"/data/icons/hicolor/symbolic/apps/*.svg; do
        [ -f "$svg" ] || continue
        install -Dm644 "$svg" "$prefix/share/icons/hicolor/symbolic/apps/$(basename "$svg")"
    done

    for po in "$ROOT"/po/*.po; do
        [ -f "$po" ] || continue
        lang="$(basename "$po" .po)"
        install -d "$prefix/share/locale/$lang/LC_MESSAGES"
        msgfmt "$po" -o "$prefix/share/locale/$lang/LC_MESSAGES/meridian.mo"
    done
}

stage_flatpak_appstream() {
    local dest="$1"
    mkdir -p \
        "$dest/share/app-info/xmls" \
        "$dest/share/app-info/icons/flatpak/64x64" \
        "$dest/share/app-info/icons/flatpak/128x128"

    cp "$ROOT/data/icons/hicolor/64x64/apps/$APP_ID.png" \
        "$dest/share/app-info/icons/flatpak/64x64/$APP_ID.png"
    cp "$ROOT/data/icons/hicolor/128x128/apps/$APP_ID.png" \
        "$dest/share/app-info/icons/flatpak/128x128/$APP_ID.png"

    python3 - "$ROOT/data/$APP_ID.metainfo.xml.in" "$APP_ID" <<'PY' | gzip -c > "$dest/share/app-info/xmls/$APP_ID.xml.gz"
import sys
path, app_id = sys.argv[1], sys.argv[2]
with open(path, "r", encoding="utf-8") as f:
    xml = f.read()
xml = xml.replace('<?xml version="1.0" encoding="UTF-8"?>\n', '')
xml = xml.replace('  <pkgname>meridian</pkgname>\n', '')
icons = (
    f'  <icon type="cached" height="128" width="128">{app_id}.png</icon>\n'
    f'  <icon type="cached" height="64" width="64">{app_id}.png</icon>\n'
    f'  <icon type="stock">{app_id}</icon>\n'
)
xml = xml.replace('  <launchable ', icons + '  <launchable ', 1)
sys.stdout.write('<?xml version="1.0" encoding="UTF-8"?>\n<components version="1.0">\n' + xml.strip() + '\n</components>\n')
PY
}

build_flatpak_bundle() {
    ensure_binary
    local arch bundle work build_dir repo_dir gpg_key gpg_b64 repo_url homepage_url branch
    arch="$(uname -m)"
    branch="stable"
    repo_url="${FLATPAK_REPO_URL:-https://meridian.martingawron.com/flatpak}"
    homepage_url="${FLATPAK_HOMEPAGE_URL:-https://github.com/megawron/meridian}"
    gpg_key="${FLATPAK_GPG_KEY:-5F5B2DAC0DFADFBA2627BB3E2654C3727A504079}"

    bundle="meridian-${RAW_VERSION}-${arch}.flatpak"

    echo "==> Packaging Flatpak repository & bundle: $bundle (branch: $branch)"
    work="$(mktemp -d)"
    build_dir="$work/build-dir"
    repo_dir="$OUT/flatpak-repo"
    rm -rf "$repo_dir"

    local runtime_ver="50"
    mkdir -p "$build_dir/files/lib" "$build_dir/var"
    cat > "$build_dir/metadata" <<EOF
[Application]
name=$APP_ID
runtime=org.gnome.Platform/$arch/$runtime_ver
sdk=org.gnome.Sdk/$arch/$runtime_ver
EOF
    stage_common "$build_dir/files"
    stage_flatpak_appstream "$build_dir/files"

    for schema in /usr/share/glib-2.0/schemas/*[Ee]volution*.gschema.xml; do
        [ -f "$schema" ] && cp "$schema" "$build_dir/files/share/glib-2.0/schemas/"
    done
    glib-compile-schemas "$build_dir/files/share/glib-2.0/schemas" 2>/dev/null || true

    if flatpak info "org.gnome.Platform/$arch/$runtime_ver" >/dev/null 2>&1; then
        flatpak run --filesystem=host --filesystem=/tmp --command=sh "org.gnome.Platform/$arch/$runtime_ver" -c '
            bin_path="$1"
            lib_dir="$2"
            while true; do
                missing=$(LD_LIBRARY_PATH="$lib_dir" ldd "$bin_path" "$lib_dir"/*.so* 2>/dev/null | awk "/not found/ {print \$1}" | sort -u)
                [ -z "$missing" ] && break
                for lib in $missing; do
                    found=""
                    for search_dir in /run/host/lib64 /run/host/lib64/evolution-data-server /run/host/usr/lib64 /run/host/usr/lib64/evolution-data-server; do
                        if [ -e "$search_dir/$lib" ]; then
                            found="$search_dir/$lib"
                            break
                        fi
                    done
                    if [ -n "$found" ]; then
                        cp -L "$found" "$lib_dir/$lib"
                    else
                        echo "Warning: Could not locate $lib on host" >&2
                        break 2
                    fi
                done
            done
        ' -- "$build_dir/files/bin/meridian" "$build_dir/files/lib"
    else
        ldd "$ROOT/target/release/meridian" | awk '/=> \// { print $3 }' | while read -r lib; do
            base="$(basename "$lib")"
            case "$base" in
                libcamel*|libedataserver*|libecal*|libebook*|libedata-book*|libebackend*|libedbus*|libical*|libgoa*|libphonenumber*|libprotobuf*|libxml2*|libboost_*|libabsl_*)
                    cp -L "$lib" "$build_dir/files/lib/$base"
                    ;;
            esac
        done
    fi

    flatpak build-finish "$build_dir" \
        --command=meridian \
        --env=LD_LIBRARY_PATH=/app/lib \
        --share=ipc \
        --share=network \
        --socket=fallback-x11 \
        --socket=wayland \
        --device=dri \
        --filesystem=xdg-config/evolution:create \
        --filesystem=xdg-cache/evolution:create \
        --filesystem=xdg-data/evolution:create \
        --filesystem=xdg-config/goa-1.0:ro \
        --filesystem=xdg-config/meridian:create \
        --filesystem=xdg-cache/meridian:create \
        --filesystem=xdg-data/meridian:create \
        --talk-name=org.gnome.evolution.dataserver.Sources5 \
        --talk-name=org.gnome.evolution.dataserver.Calendar8 \
        --talk-name=org.gnome.evolution.dataserver.AddressBook10 \
        --talk-name=org.gnome.evolution.dataserver.Subprocess.Backend.* \
        --talk-name=org.gnome.OnlineAccounts \
        --talk-name=org.gnome.Settings \
        --talk-name=org.gnome.ControlCenter \
        --talk-name=org.freedesktop.secrets >/dev/null

    local gpg_args=()
    if gpg --list-secret-keys "$gpg_key" >/dev/null 2>&1; then
        gpg_args=(--gpg-sign="$gpg_key")
    fi

    flatpak build-export --disable-fsync --update-appstream "${gpg_args[@]}" "$repo_dir" "$build_dir" "$branch" >/dev/null
    flatpak build-update-repo \
        --title="Meridian Flatpak Repository" \
        --comment="Official Flatpak repository for Meridian" \
        --description="Official Flatpak repository for Meridian (Mail, Calendar, Contacts, and Tasks for GNOME)" \
        --homepage="$homepage_url" \
        --default-branch="$branch" \
        --generate-static-deltas \
        --static-delta-jobs=1 \
        "${gpg_args[@]}" \
        "$repo_dir" >/dev/null

    local bundle_gpg_args=()
    if gpg --list-secret-keys "$gpg_key" >/dev/null 2>&1; then
        gpg --export "$gpg_key" > "$work/repo.gpg"
        bundle_gpg_args=(--gpg-keys="$work/repo.gpg")
        gpg_b64="$(base64 -w0 < "$work/repo.gpg")"
        cat > "$repo_dir/$APP_ID.flatpakref" <<EOF
[Flatpak Ref]
Title=Meridian
Name=$APP_ID
Branch=$branch
Url=$repo_url
Homepage=$homepage_url
RuntimeRepo=https://dl.flathub.org/repo/flathub.flatpakrepo
IsRuntime=false
GPGKey=$gpg_b64
EOF

        cat > "$repo_dir/meridian.flatpakrepo" <<EOF
[Flatpak Repo]
Title=Meridian
Url=$repo_url
Homepage=$homepage_url
Comment=Official Flatpak repository for Meridian
Description=Official Flatpak repository for Meridian (Mail, Calendar, Contacts, and Tasks for GNOME)
DefaultBranch=$branch
GPGKey=$gpg_b64
EOF
    fi

    flatpak build-bundle "$repo_dir" "$OUT/$bundle" "$APP_ID" "$branch" \
        --repo-url="$repo_url" \
        --runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo \
        "${bundle_gpg_args[@]}"
    rm -rf "$work"
    echo "==> Flatpak bundle ready: $OUT/$bundle"
    echo "==> Flatpakref ready:     $repo_dir/$APP_ID.flatpakref"
    echo "==> Flatpak repo ready:   $repo_dir"
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
Summary:        Mail, calendar, contacts, and tasks for GNOME
License:        GPL-3.0-or-later
URL:            https://github.com/megawron/meridian
Source0:        %{name}-%{version}-bin.tar
Requires:       evolution-data-server
Recommends:     gnome-keyring

%description
Meridian is an all-in-one mail, calendar, contacts, and tasks application
designed for the GNOME desktop, built with Rust, GTK 4, libadwaita, and
WebKitGTK.

Built on top of Evolution Data Server (EDS), Meridian works out of the box
with your existing system accounts, local folders, and GNOME Online Accounts.

%prep
%setup -q -n %{name}-%{version}-bin

%install
cp -a usr %{buildroot}/usr

%post
glib-compile-schemas %{_datadir}/glib-2.0/schemas >/dev/null 2>&1 || :
gtk-update-icon-cache -q -t -f %{_datadir}/icons/hicolor >/dev/null 2>&1 || :
update-desktop-database -q %{_datadir}/applications >/dev/null 2>&1 || :

%postun
if [ \$1 -eq 0 ] ; then
    glib-compile-schemas %{_datadir}/glib-2.0/schemas >/dev/null 2>&1 || :
    gtk-update-icon-cache -q -t -f %{_datadir}/icons/hicolor >/dev/null 2>&1 || :
    update-desktop-database -q %{_datadir}/applications >/dev/null 2>&1 || :
fi

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
%{_datadir}/icons/hicolor/symbolic/apps/*.svg
%{_datadir}/locale/*/LC_MESSAGES/meridian.mo

%changelog
* Fri Nov 06 2026 Martin Gawron <martin@martingawron.com> - ${PKG_VERSION}-1
- Initial public beta release of Meridian
- Unified mail workspace with threaded conversations, fast full-text search, OpenPGP and S/MIME support, and sandboxed HTML preview
- Multi-view calendar supporting Day, Work Week, Week, Month, Agenda, and Year views
- Integrated Contacts, Tasks, and Memos with Evolution Data Server (EDS) and GNOME Online Accounts integration
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
    stage_flatpak_appstream "$fp_stage"

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

merge_flatpak_repos() {
    # Merges the refs of several single-architecture Flatpak repositories into
    # the first one and regenerates the signed summary, so that one repository
    # serves every architecture.
    local dest="$1"; shift
    local gpg_key="${FLATPAK_GPG_KEY:-5F5B2DAC0DFADFBA2627BB3E2654C3727A504079}"
    local homepage_url="${FLATPAK_HOMEPAGE_URL:-https://github.com/megawron/meridian}"
    local branch="stable"
    local src ref
    for src in "$@"; do
        while read -r ref; do
            [ -n "$ref" ] || continue
            echo "==> Merging $ref from $src"
            ostree pull-local --repo="$dest" "$src" "$ref" >/dev/null
        done < <(ostree refs --repo="$src")
    done

    local gpg_args=()
    if gpg --list-secret-keys "$gpg_key" >/dev/null 2>&1; then
        gpg_args=(--gpg-sign="$gpg_key")
        # Make sure every merged commit carries a signature.
        while read -r ref; do
            case "$ref" in app/*) flatpak build-sign "${gpg_args[@]}" "$dest" "${ref#app/}" 2>/dev/null || true ;; esac
        done < <(ostree refs --repo="$dest")
    fi

    flatpak build-update-repo \
        --title="Meridian Flatpak Repository" \
        --comment="Official Flatpak repository for Meridian" \
        --description="Official Flatpak repository for Meridian (Mail, Calendar, Contacts, and Tasks for GNOME)" \
        --homepage="$homepage_url" \
        --default-branch="$branch" \
        --generate-static-deltas \
        --static-delta-jobs=1 \
        "${gpg_args[@]}" \
        "$dest" >/dev/null
    echo "==> Refs in merged repository:"
    ostree refs --repo="$dest"
}

publish_gh_pages() {
    local repo_dir="$OUT/flatpak-repo"
    if [ ! -d "$repo_dir/objects" ]; then
        build_flatpak_bundle
    fi
    echo "==> Publishing Flatpak repository to gh-pages branch..."
    local pages_work
    pages_work="$(mktemp -d)"
    git -C "$pages_work" init -b gh-pages >/dev/null
    git -C "$pages_work" config user.name "$(git -C "$ROOT" config user.name || echo 'Martin Gawron')"
    git -C "$pages_work" config user.email "$(git -C "$ROOT" config user.email || echo 'martin@martingawron.com')"

    touch "$pages_work/.nojekyll"
    local cname="${FLATPAK_CNAME:-meridian.martingawron.com}"
    if [ -n "$cname" ]; then
        echo "$cname" > "$pages_work/CNAME"
    fi
    mkdir -p "$pages_work/flatpak"
    cp -a "$repo_dir/." "$pages_work/flatpak/"
    rm -f "$pages_work/flatpak/.lock"
    [ -f "$repo_dir/$APP_ID.flatpakref" ] && cp "$repo_dir/$APP_ID.flatpakref" "$pages_work/$APP_ID.flatpakref"

    cat > "$pages_work/index.html" <<EOF
<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Meridian — Flatpak Repository</title>
  <style>
    :root { color-scheme: light dark; }
    body { font-family: system-ui, -apple-system, sans-serif; max-width: 720px; margin: 4rem auto; padding: 0 1.5rem; line-height: 1.6; }
    pre { background: rgba(127,127,127,0.12); padding: 1rem; border-radius: 8px; overflow-x: auto; }
    a.btn { display: inline-block; background: #3584e4; color: #fff; text-decoration: none; padding: 0.65rem 1.2rem; border-radius: 8px; font-weight: 600; margin-right: 0.75rem; }
  </style>
</head>
<body>
  <h1>Meridian</h1>
  <p>Mail, calendar, contacts, and tasks for GNOME.</p>
  <p>
    <a class="btn" href="flatpak/$APP_ID.flatpakref">Install Flatpak (.flatpakref)</a>
    <a href="https://github.com/megawron/meridian">GitHub Repository</a>
  </p>
  <h2>Install via Terminal</h2>
  <pre><code>flatpak install --user --from ${FLATPAK_REPO_URL:-https://meridian.martingawron.com/flatpak}/$APP_ID.flatpakref</code></pre>
</body>
</html>
EOF

    git -C "$pages_work" add -A
    git -C "$pages_work" commit -m "chore: publish Flatpak repository ($TAG)" >/dev/null
    local remote_url
    # CI passes an authenticated URL; locally the origin remote is used.
    remote_url="${GH_PAGES_REMOTE:-$(git -C "$ROOT" remote get-url origin)}"
    git -C "$pages_work" push --force "$remote_url" gh-pages:gh-pages
    rm -rf "$pages_work"
    echo "==> Published Flatpak repository to gh-pages!"
}

build_local_all() {
    rm -rf "$OUT"/*
    build_flatpak_bundle
    if command -v rpmbuild >/dev/null 2>&1; then
        build_rpm
    fi
    ( cd "$OUT" && find . -maxdepth 1 -type f ! -name SHA256SUMS.txt -printf '%f\n' | sort | xargs sha256sum > SHA256SUMS.txt )
    echo "==> All local release packages built in $OUT:"
    ls -lh "$OUT"
}

case "$WHAT" in
    local) build_local_all ;;
    upload)
        build_local_all
        publish_gh_pages
        echo "==> Uploading packages to GitHub release $TAG..."
        mapfile -t release_files < <(find "$OUT" -maxdepth 1 -type f | sort)
        gh release upload "$TAG" "${release_files[@]}" --clobber
        echo "==> Uploaded successfully to $TAG!"
        ;;
    publish-repo) publish_gh_pages ;;
    merge-repos) shift; merge_flatpak_repos "$@" ;;
    flatpak) build_flatpak_bundle ;;
    rpm) build_rpm ;;
    flatpak-stage) stage_flatpak ;;
    *) echo "Usage: $0 {local|upload|publish-repo|merge-repos|flatpak|rpm} [tag]" >&2; exit 1 ;;
esac
