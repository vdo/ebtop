use std::collections::HashMap;
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use libbpf_cargo::SkeletonBuilder;

const SRC: &str = "src/bpf/ebtop.bpf.c";

fn main() {
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR not set"));
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let (bpf_arch, multiarch) = match target_arch.as_str() {
        "aarch64" => ("arm64", "aarch64-linux-gnu"),
        _ => ("x86", "x86_64-linux-gnu"),
    };

    let mut clang_args = vec![format!("-D__TARGET_ARCH_{bpf_arch}"), "-Wall".into()];
    // Debian/Ubuntu keep <asm/types.h> under a multiarch directory that
    // clang's BPF target doesn't search by default.
    let multiarch_inc = Path::new("/usr/include").join(multiarch);
    if multiarch_inc.join("asm").is_dir() {
        clang_args.push(format!("-I{}", multiarch_inc.display()));
    }

    SkeletonBuilder::new()
        .source(SRC)
        .clang_args(clang_args)
        .build_and_generate(out_dir.join("ebtop.skel.rs"))
        .expect("failed to build BPF skeleton");

    write_syscall_table(&out_dir.join("syscalls.rs"), &target_arch, multiarch);
    write_theme_table(&out_dir.join("themes.rs"));

    println!("cargo:rerun-if-changed={SRC}");
    println!("cargo:rerun-if-changed=src/bpf/ebtop.h");
}

/// Embeds syscall number -> name from the build host's UAPI headers, so the
/// binary doesn't need kernel headers installed where it runs.
fn write_syscall_table(out: &Path, target_arch: &str, multiarch: &str) {
    let candidates: Vec<PathBuf> = match target_arch {
        "x86_64" => {
            vec!["/usr/include/asm/unistd_64.h".into(), format!("/usr/include/{multiarch}/asm/unistd_64.h").into()]
        }
        _ => vec!["/usr/include/asm-generic/unistd.h".into()],
    };

    // `#define __NR_foo <number | other __NR symbol>`; asm-generic aliases
    // some numbers through __NR3264_* symbols.
    let mut symbols: HashMap<String, String> = HashMap::new();
    if let Some(path) = candidates.iter().find(|p| p.is_file()) {
        println!("cargo:rerun-if-changed={}", path.display());
        let text = fs::read_to_string(path).expect("reading syscall header");
        for line in text.lines() {
            let mut it = line.split_whitespace();
            if let (Some("#define"), Some(name), Some(value), None) = (it.next(), it.next(), it.next(), it.next())
                && name.starts_with("__NR")
            {
                symbols.insert(name.to_string(), value.to_string());
            }
        }
    } else {
        println!("cargo:warning=no syscall header found; syscalls will be shown by number");
    }

    let mut table: Vec<(u32, &str)> = symbols
        .iter()
        .filter_map(|(name, value)| Some((resolve(&symbols, value)?, name.strip_prefix("__NR_")?)))
        .filter(|(_, name)| *name != "syscalls")
        .collect();
    table.sort();
    table.dedup_by_key(|(n, _)| *n);

    let mut src = String::from("pub const SYSCALLS: &[(u32, &str)] = &[\n");
    for (n, name) in table {
        writeln!(src, "    ({n}, {name:?}),").unwrap();
    }
    src.push_str("];\n");
    fs::write(out, src).expect("writing syscall table");
}

/// Follows `#define` aliases until a number is reached.
fn resolve<'a>(symbols: &'a HashMap<String, String>, mut v: &'a str) -> Option<u32> {
    for _ in 0..4 {
        if let Ok(n) = v.parse() {
            return Some(n);
        }
        v = symbols.get(v)?;
    }
    None
}

/// Embeds the bundled btop themes (themes/*.theme) as (name, contents).
fn write_theme_table(out: &Path) {
    let dir = Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap()).join("themes");
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut themes: Vec<PathBuf> = fs::read_dir(&dir)
        .expect("reading themes/")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "theme"))
        .collect();
    themes.sort();

    let mut src = String::from("pub const THEMES: &[(&str, &str)] = &[\n");
    for path in themes {
        let name = path.file_stem().unwrap().to_string_lossy();
        writeln!(src, "    ({name:?}, include_str!({:?})),", path.display().to_string()).unwrap();
    }
    src.push_str("];\n");
    fs::write(out, src).expect("writing theme table");
}
