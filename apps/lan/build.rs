use std::path::PathBuf;

fn main() {
    // Scan the whole workspace for Tailwind classes: pages live in
    // apps/lan but the media app's components live in crates/.
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    match topcoat::tailwind::BuildConfig::new().cwd(workspace).render() {
        Ok(_) => {}
        Err(error) => panic!("tailwind build failed: {error}"),
    }
}