// SPDX-License-Identifier: GPL-3.0-or-later

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo::rustc-check-cfg=cfg(eds_camel_bridge)");
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());

    let schema_name = "com.martingawron.Meridian.gschema.xml";
    let source = manifest_dir.join("../data").join(schema_name);
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
