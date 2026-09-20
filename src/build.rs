// SPDX-License-Identifier: GPL-3.0-or-later

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rustc-check-cfg=cfg(eds_camel_bridge)");
    println!("cargo:rustc-check-cfg=cfg(eds_ecal_bridge)");
    println!("cargo:rustc-check-cfg=cfg(eds_ebook_bridge)");
    println!("cargo:rustc-check-cfg=cfg(macos_bridge)");

    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    prepare_settings_schema(&manifest_dir, &out_dir);
    compile_locales(&manifest_dir, &out_dir);

    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        compile_macos_bridge(&manifest_dir, &out_dir);
        return;
    }

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux") {
        return;
    }

    // Keep pkg-config from emitting link flags immediately.  The bridge is a
    // static archive and must appear before its Camel/EDS dependencies in the
    // final linker command, otherwise GNU ld can discard the dependencies
    // before it sees the bridge's undefined symbols.
    let camel = pkg_config::Config::new()
        .cargo_metadata(false)
        .probe("camel-1.2")
        .expect(
            "camel-1.2 development files are required on Linux; install evolution-data-server-devel",
        );
    let eds = pkg_config::Config::new()
        .cargo_metadata(false)
        .probe("libedataserver-1.2")
        .expect("libedataserver-1.2 development files are required on Linux; install evolution-data-server-devel");
    let ecal = pkg_config::Config::new()
        .cargo_metadata(false)
        .probe("libecal-2.0")
        .expect("libecal-2.0 development files are required on Linux; install evolution-data-server-devel");
    let ebook = pkg_config::Config::new()
        .cargo_metadata(false)
        .probe("libebook-1.2")
        .expect("libebook-1.2 development files are required on Linux; install evolution-data-server-devel");
    let ebook_contacts = pkg_config::Config::new()
        .cargo_metadata(false)
        .probe("libebook-contacts-1.2")
        .expect("libebook-contacts-1.2 development files are required on Linux; install evolution-data-server-devel");

    let bridge = manifest_dir.join("../native/eds_camel_bridge.c");
    let object = out_dir.join("eds_camel_bridge.o");
    let archive = out_dir.join("libevolution_eds_camel_bridge.a");
    let compiler = env::var_os("CC").unwrap_or_else(|| "cc".into());
    let archiver = env::var_os("AR").unwrap_or_else(|| "ar".into());
    let mut compile = Command::new(&compiler);
    compile
        .arg("-std=c11")
        .arg("-fPIC")
        .arg("-c")
        .arg(&bridge)
        .arg("-o")
        .arg(&object)
        .arg("-I")
        .arg(manifest_dir.join("../native"));
    let has_dup_uids = version_at_least(&camel.version, 3, 58);
    compile.arg(format!(
        "-DEDS_BRIDGE_HAS_DUP_UIDS={}",
        if has_dup_uids { 1 } else { 0 }
    ));
    for include_path in camel.include_paths.iter().chain(eds.include_paths.iter()) {
        compile.arg("-I").arg(include_path);
    }
    for (name, value) in camel.defines.iter().chain(eds.defines.iter()) {
        compile.arg(match value {
            Some(value) => format!("-D{name}={value}"),
            None => format!("-D{name}"),
        });
    }
    let compile_status = compile
        .status()
        .expect("could not execute the C compiler for the EDS/Camel bridge");
    assert!(
        compile_status.success(),
        "the EDS/Camel bridge failed to compile"
    );

    let archive_status = Command::new(&archiver)
        .arg("crus")
        .arg(&archive)
        .arg(&object)
        .status()
        .expect("could not execute ar for the EDS/Camel bridge");
    assert!(
        archive_status.success(),
        "the EDS/Camel bridge failed to create a static archive"
    );

    let ecal_bridge = manifest_dir.join("../native/eds_ecal_bridge.c");
    let ecal_object = out_dir.join("eds_ecal_bridge.o");
    let ecal_archive = out_dir.join("libevolution_eds_ecal_bridge.a");
    let mut ecal_compile = Command::new(&compiler);
    ecal_compile
        .arg("-std=c11")
        .arg("-fPIC")
        .arg("-c")
        .arg(&ecal_bridge)
        .arg("-o")
        .arg(&ecal_object)
        .arg("-I")
        .arg(manifest_dir.join("../native"));
    for include_path in ecal
        .include_paths
        .iter()
        .chain(eds.include_paths.iter())
        .chain(camel.include_paths.iter())
    {
        ecal_compile.arg("-I").arg(include_path);
    }
    for (name, value) in ecal
        .defines
        .iter()
        .chain(eds.defines.iter())
        .chain(camel.defines.iter())
    {
        ecal_compile.arg(match value {
            Some(value) => format!("-D{name}={value}"),
            None => format!("-D{name}"),
        });
    }
    let ecal_compile_status = ecal_compile
        .status()
        .expect("could not execute the C compiler for the EDS/ECal bridge");
    assert!(
        ecal_compile_status.success(),
        "the EDS/ECal bridge failed to compile"
    );

    let ecal_archive_status = Command::new(&archiver)
        .arg("crus")
        .arg(&ecal_archive)
        .arg(&ecal_object)
        .status()
        .expect("could not execute ar for the EDS/ECal bridge");
    assert!(
        ecal_archive_status.success(),
        "the EDS/ECal bridge failed to create a static archive"
    );

    let ebook_bridge = manifest_dir.join("../native/eds_ebook_bridge.c");
    let ebook_object = out_dir.join("eds_ebook_bridge.o");
    let ebook_archive = out_dir.join("libevolution_eds_ebook_bridge.a");
    let mut ebook_compile = Command::new(&compiler);
    ebook_compile
        .arg("-std=c11")
        .arg("-fPIC")
        .arg("-c")
        .arg(&ebook_bridge)
        .arg("-o")
        .arg(&ebook_object)
        .arg("-I")
        .arg(manifest_dir.join("../native"));
    for include_path in ebook
        .include_paths
        .iter()
        .chain(ebook_contacts.include_paths.iter())
        .chain(eds.include_paths.iter())
    {
        ebook_compile.arg("-I").arg(include_path);
    }
    for (name, value) in ebook
        .defines
        .iter()
        .chain(ebook_contacts.defines.iter())
        .chain(eds.defines.iter())
    {
        ebook_compile.arg(match value {
            Some(value) => format!("-D{name}={value}"),
            None => format!("-D{name}"),
        });
    }
    let ebook_compile_status = ebook_compile
        .status()
        .expect("could not execute the C compiler for the EDS/EBook bridge");
    assert!(
        ebook_compile_status.success(),
        "the EDS/EBook bridge failed to compile"
    );

    let ebook_archive_status = Command::new(&archiver)
        .arg("crus")
        .arg(&ebook_archive)
        .arg(&ebook_object)
        .status()
        .expect("could not execute ar for the EDS/EBook bridge");
    assert!(
        ebook_archive_status.success(),
        "the EDS/EBook bridge failed to create a static archive"
    );

    // Keep the generated object out of incremental rebuild surprises while
    // retaining the archive in OUT_DIR for Cargo's linker invocation.
    let _ = fs::remove_file(object);
    let _ = fs::remove_file(ecal_object);
    let _ = fs::remove_file(ebook_object);
    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=evolution_eds_camel_bridge");
    println!("cargo:rustc-link-lib=static=evolution_eds_ecal_bridge");
    println!("cargo:rustc-link-lib=static=evolution_eds_ebook_bridge");
    println!("cargo:rustc-link-lib=sqlite3");
    emit_link_metadata(&camel);
    emit_link_metadata(&eds);
    emit_link_metadata(&ecal);
    emit_link_metadata(&ebook);
    emit_link_metadata(&ebook_contacts);
    println!("cargo:rerun-if-changed={}", bridge.display());
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("../native/eds_camel_bridge.h").display()
    );
    println!("cargo:rerun-if-changed={}", ecal_bridge.display());
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir
            .join("../native/eds_ecal_snapshot_cache.h")
            .display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("../native/eds_ecal_bridge.h").display()
    );
    println!("cargo:rerun-if-changed={}", ebook_bridge.display());
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("../native/eds_ebook_bridge.h").display()
    );

    println!("cargo:rustc-cfg=eds_camel_bridge");
    println!("cargo:rustc-cfg=eds_ecal_bridge");
    println!("cargo:rustc-cfg=eds_ebook_bridge");
}

/// Make `cargo run` behave like the installed Meson build for developers.
/// Meson installs `gschemas.compiled`, but a direct Cargo build otherwise has
/// no way to find the repository's XML schema.  When GLib is available, keep a
/// compiled copy in Cargo's OUT_DIR and let AppSettings use it as a fallback.
/// Production installs still use the normal system schema search path.
fn prepare_settings_schema(manifest_dir: &PathBuf, out_dir: &PathBuf) {
    let schema_name = "com.martingawron.Meridian.gschema.xml";
    let source = if manifest_dir.join("data").join(schema_name).is_file() {
        manifest_dir.join("data").join(schema_name)
    } else {
        manifest_dir.join("../data").join(schema_name)
    };
    let schema_dir = out_dir.join("schemas");
    if !source.is_file() || fs::create_dir_all(&schema_dir).is_err() {
        return;
    }
    let target = schema_dir.join(schema_name);
    if fs::copy(&source, &target).is_err() {
        return;
    }

    let status = Command::new("glib-compile-schemas")
        .arg(&schema_dir)
        .status();
    if status.is_ok_and(|status| status.success()) {
        println!(
            "cargo:rustc-env=MERIDIAN_SCHEMA_DIR={}",
            schema_dir.display()
        );
        println!(
            "cargo:rustc-env=EVOLUTION_GTK4_SCHEMA_DIR={}",
            schema_dir.display()
        );
    }
    println!("cargo:rerun-if-changed={}", source.display());
}

fn compile_locales(manifest_dir: &PathBuf, out_dir: &PathBuf) {
    let po_dir = manifest_dir.join("../po");
    if !po_dir.is_dir() {
        return;
    }

    let out_locale_dir = out_dir.join("locale");
    let repo_locale_dir = manifest_dir.join("../data/locale");

    println!("cargo:rerun-if-changed={}", po_dir.display());

    if let Ok(entries) = fs::read_dir(&po_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "po") {
                println!("cargo:rerun-if-changed={}", path.display());
                if let Some(file_stem) = path.file_stem().and_then(|s| s.to_str()) {
                    let lang = file_stem;

                    let out_lang_dir = out_locale_dir.join(lang).join("LC_MESSAGES");
                    let _ = fs::create_dir_all(&out_lang_dir);
                    let out_mo = out_lang_dir.join("meridian.mo");

                    let repo_lang_dir = repo_locale_dir.join(lang).join("LC_MESSAGES");
                    let _ = fs::create_dir_all(&repo_lang_dir);
                    let repo_mo = repo_lang_dir.join("meridian.mo");

                    let status = Command::new("msgfmt")
                        .arg("-o")
                        .arg(&out_mo)
                        .arg(&path)
                        .status();

                    if status.is_ok_and(|s| s.success()) {
                        let _ = fs::copy(&out_mo, &repo_mo);
                    }
                }
            }
        }
    }

    println!(
        "cargo:rustc-env=MERIDIAN_LOCALEDIR={}",
        out_locale_dir.display()
    );
    println!(
        "cargo:rustc-env=EVOLUTION_GTK4_LOCALEDIR={}",
        out_locale_dir.display()
    );
}

fn emit_link_metadata(library: &pkg_config::Library) {
    for path in &library.link_paths {
        println!("cargo:rustc-link-search=native={}", path.display());
    }
    for path in &library.framework_paths {
        println!("cargo:rustc-link-search=framework={}", path.display());
    }
    for framework in &library.frameworks {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
    for library_name in &library.libs {
        println!("cargo:rustc-link-lib={library_name}");
    }
    for args in &library.ld_args {
        if !args.is_empty() {
            println!("cargo:rustc-link-arg=-Wl,{}", args.join(","));
        }
    }
}

fn version_at_least(version: &str, major: u32, minor: u32) -> bool {
    let mut parts = version.split('.');
    let parsed_major = parts
        .next()
        .and_then(|part| part.parse::<u32>().ok())
        .unwrap_or(major);
    let parsed_minor = parts
        .next()
        .and_then(|part| part.parse::<u32>().ok())
        .unwrap_or(minor);

    (parsed_major, parsed_minor) >= (major, minor)
}

fn compile_macos_bridge(manifest_dir: &std::path::Path, out_dir: &std::path::Path) {
    let bridge = manifest_dir.join("../native/macos_bridge.m");
    let object = out_dir.join("macos_bridge.o");
    let archive = out_dir.join("libevolution_macos_bridge.a");
    let compiler = env::var_os("CC").unwrap_or_else(|| "clang".into());
    let archiver = env::var_os("AR").unwrap_or_else(|| "ar".into());

    let mut compile = Command::new(&compiler);
    compile
        .arg("-fobjc-arc")
        .arg("-c")
        .arg(&bridge)
        .arg("-o")
        .arg(&object)
        .arg("-I")
        .arg(manifest_dir.join("../native"));

    let compile_status = compile
        .status()
        .expect("could not execute compiler for macOS bridge");
    assert!(
        compile_status.success(),
        "the macOS bridge failed to compile"
    );

    let archive_status = Command::new(&archiver)
        .arg("crus")
        .arg(&archive)
        .arg(&object)
        .status()
        .expect("could not execute ar for macOS bridge");
    assert!(
        archive_status.success(),
        "the macOS bridge failed to create static archive"
    );

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=evolution_macos_bridge");
    println!("cargo:rustc-link-lib=framework=Foundation");
    println!("cargo:rustc-link-lib=framework=EventKit");
    println!("cargo:rustc-link-lib=framework=Contacts");
    println!("cargo:rustc-link-lib=framework=AppKit");
    println!("cargo:rustc-link-lib=framework=Security");

    let plist_path = manifest_dir.join("../data/Info.plist");
    if plist_path.exists() {
        println!(
            "cargo:rustc-link-arg=-Wl,-sectcreate,__TEXT,__info_plist,{}",
            plist_path.display()
        );
    }
    println!("cargo:rustc-cfg=macos_bridge");
    println!("cargo:rerun-if-changed={}", bridge.display());
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("../native/macos_bridge.h").display()
    );
    println!("cargo:rerun-if-changed={}", plist_path.display());
}
