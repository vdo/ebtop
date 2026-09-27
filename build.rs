use std::env;
use std::path::PathBuf;

use libbpf_cargo::SkeletonBuilder;

const SRC: &str = "src/bpf/ebtop.bpf.c";

fn main() {
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR not set")).join("ebtop.skel.rs");
    let arch = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => "arm64",
        _ => "x86",
    };

    SkeletonBuilder::new()
        .source(SRC)
        .clang_args([format!("-D__TARGET_ARCH_{arch}"), "-Wall".into()])
        .build_and_generate(&out)
        .expect("failed to build BPF skeleton");

    println!("cargo:rerun-if-changed={SRC}");
    println!("cargo:rerun-if-changed=src/bpf/ebtop.h");
}
