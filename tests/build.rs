// SPDX-License-Identifier: GPL-3.0-or-later

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());

    let source = manifest_dir.join("../data/org.gnome.Evolution4.gschema.xml");
    let schema_dir = out_dir.join("schemas");
    if !source.is_file() || fs::create_dir_all(&schema_dir).is_err() {
        return;
    }
    let target = schema_dir.join("org.gnome.Evolution4.gschema.xml");
    if fs::copy(&source, &target).is_err() {
        return;
    }

    let status = Command::new("glib-compile-schemas")
        .arg(&schema_dir)
        .status();
    if status.is_ok_and(|status| status.success()) {
        println!(
            "cargo:rustc-env=EVOLUTION_GTK4_SCHEMA_DIR={}",
            schema_dir.display()
        );
    }
    println!("cargo:rerun-if-changed={}", source.display());
}
