use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    if Path::new("../../.git/refs/heads").exists() {
        println!("cargo:rerun-if-changed=../../.git/refs/heads");
    }

    let pkg_version = env!("CARGO_PKG_VERSION");
    let manifest_dir = env!("CARGO_MANIFEST_DIR");

    let git_hash = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(manifest_dir)
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !s.is_empty() {
                    Some(s)
                } else {
                    None
                }
            } else {
                None
            }
        });

    let version_with_hash = match git_hash {
        Some(hash) => format!("{pkg_version}+{hash}"),
        None => pkg_version.to_string(),
    };

    println!("cargo:rustc-env=ERTANGLE_VERSION_WITH_HASH={version_with_hash}");
}
