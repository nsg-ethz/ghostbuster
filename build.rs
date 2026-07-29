// This build script extracts the commit rev and adds it as an environment variable to rustc, so
// the program can get it.

use std::process::Command;
fn main() {
    let git_hash = Command::new("git")
        .args(&["rev-parse", "HEAD"])
        .output()
        .ok()
        .and_then(|x| String::from_utf8(x.stdout).ok())
        .unwrap_or_default();
    println!("cargo:rustc-env=GIT_HASH={git_hash}")
}
