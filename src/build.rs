// SPDX-License-Identifier: GPL-3.0-or-later

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rustc-check-cfg=cfg(eds_camel_bridge)");

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

    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let bridge = manifest_dir.join("../native/eds_camel_bridge.c");
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let object = out_dir.join("eds_camel_bridge.o");
    let archive = out_dir.join("libevolution_eds_camel_bridge.a");
    let compiler = env::var_os("CC").unwrap_or_else(|| "cc".into());
    let archiver = env::var_os("AR").unwrap_or_else(|| "ar".into());
    let mut compile = Command::new(compiler);
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

    let archive_status = Command::new(archiver)
        .arg("crus")
        .arg(&archive)
        .arg(&object)
        .status()
        .expect("could not execute ar for the EDS/Camel bridge");
    assert!(
        archive_status.success(),
        "the EDS/Camel bridge failed to create a static archive"
    );

    // Keep the generated object out of incremental rebuild surprises while
    // retaining the archive in OUT_DIR for Cargo's linker invocation.
    let _ = fs::remove_file(object);
    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=evolution_eds_camel_bridge");
    emit_link_metadata(&camel);
    emit_link_metadata(&eds);
    println!("cargo:rerun-if-changed={}", bridge.display());
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("../native/eds_camel_bridge.h").display()
    );

    println!("cargo:rustc-cfg=eds_camel_bridge");
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
