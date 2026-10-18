//! The build's version: the workspace version, the commit it was built
//! from, and `-dirty` when the code the binaries are made of differs from
//! that commit (`1.39.0+4360f7c`, `1.39.0+4360f7c-dirty`). Without git
//! (a source archive), the plain version.

use std::path::Path;
use std::process::Command;

fn git(root: &Path, args: &[&str]) -> Option<std::process::Output> {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()
}

fn main() {
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let root = Path::new(&manifest).join("../..");
    // A commit, a checkout, a staged or edited file reruns this.
    let git_dir = root.join(".git");
    for p in [
        git_dir.join("HEAD"),
        git_dir.join("index"),
        git_dir.join("packed-refs"),
    ] {
        println!("cargo:rerun-if-changed={}", p.display());
    }
    if let Ok(head) = std::fs::read_to_string(git_dir.join("HEAD"))
        && let Some(r) = head.trim().strip_prefix("ref: ")
    {
        println!("cargo:rerun-if-changed={}", git_dir.join(r).display());
    }
    for p in ["crates", "Cargo.toml", "Cargo.lock"] {
        println!("cargo:rerun-if-changed={}", root.join(p).display());
    }
    let sha = git(&root, &["rev-parse", "--short=7", "HEAD"])
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty());
    let full = match sha {
        Some(sha) => {
            let dirty = git(
                &root,
                &[
                    "diff",
                    "--quiet",
                    "HEAD",
                    "--",
                    "crates",
                    "Cargo.toml",
                    "Cargo.lock",
                ],
            )
            .is_some_and(|o| o.status.code() == Some(1));
            format!("{version}+{sha}{}", if dirty { "-dirty" } else { "" })
        }
        None => version,
    };
    println!("cargo:rustc-env=LUNA_BUILD_VERSION={full}");
}
