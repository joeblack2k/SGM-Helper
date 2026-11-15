use std::process::Command;

/// The build's own identity, so a state marker can tell one build from another.
///
/// The package version is the same for every fork build (0.4.16), so a marker
/// stamped with it never expires when the binary is replaced by a fixed one.
/// A release workflow can set SGM_BUILD_ID; otherwise the commit is used.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=SGM_BUILD_ID");
    // Paths in a build script resolve against the crate directory, so a bare
    // `.git/HEAD` points at helpers/<crate>/.git/HEAD, which does not exist:
    // an incremental build keeps the identity of whatever commit it first saw.
    // Ask git for its directory and watch the files it actually reads.
    for path in git_watch_paths() {
        println!("cargo:rerun-if-changed={path}");
    }

    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "unknown".to_string());
    let revision = std::env::var("SGM_BUILD_ID")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            Command::new("git")
                .args(["rev-parse", "--short=12", "HEAD"])
                .output()
                .ok()
                .filter(|output| output.status.success())
                .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
                .filter(|value| !value.is_empty())
        });

    let identity = match revision {
        Some(revision) => format!("{version}+{revision}"),
        None => {
            // Without a revision the identity is the same for every build, and
            // a state marker written by one build would look current to the
            // next -- the expiry stops working silently. Say so at build time;
            // a release workflow should set SGM_BUILD_ID.
            println!(
                "cargo:warning=no SGM_BUILD_ID and no git revision: build identity is the package \
                 version alone, so state markers will not expire across builds"
            );
            version
        }
    };
    println!("cargo:rustc-env=SGM_BUILD_ID={identity}");
}

/// The files git consults to answer `rev-parse HEAD`: the resolved git
/// directory's HEAD, the ref HEAD names, and packed-refs.
fn git_watch_paths() -> Vec<String> {
    let git_dir = git_output(&["rev-parse", "--absolute-git-dir"]);
    let Some(git_dir) = git_dir else {
        return Vec::new();
    };
    let mut paths = vec![format!("{git_dir}/HEAD"), format!("{git_dir}/packed-refs")];
    if let Some(reference) = git_output(&["symbolic-ref", "-q", "HEAD"]) {
        paths.push(format!("{git_dir}/{reference}"));
    }
    paths
}

fn git_output(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() { None } else { Some(value) }
}
