# EDS GIR adapter generation

The application keeps the original Evolution Data Server authoritative. The
Linux mail path now uses a small C ABI bridge compiled against the installed
`camel-1.2` and `libedataserver-1.2` development headers. It reads enabled
EDS `Mail Account` sources, configures the matching Camel provider, uses EDS
credentials/GOA OAuth, and returns owned RFC-822 records to Rust. It never
writes EDS source data or replaces the EDS registry.

The GIR specifications below remain the planned Rust adapter path for the
remaining Camel/ECal/EBook mutations. They are generated from the installed
Evolution Data Server GIR files rather than checked-in, hand-written FFI.

Run:

```sh
./scripts/generate_gir_bindings.sh
```

The script requires `Camel-1.2.gir`, `EDataServer-1.2.gir`, `ECal-2.0.gir`
and `EBook-1.2.gir` from `evolution-data-server-devel`. Generated output is
kept below the build/target tree and is not user data. The generated crates
must be reviewed against the installed EDS version before enabling the
`eds-gir` application feature.

On Linux, a normal Cargo/Meson build also needs the EDS development packages;
the build script compiles the native mail bridge automatically. On macOS the
bridge is omitted and the tested local EML/Maildir/mbox fallback remains
available.
