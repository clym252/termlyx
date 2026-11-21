// `sqlx::migrate!` embeds `migrations/` at compile time; re-run when it changes.
fn main() {
    println!("cargo:rerun-if-changed=migrations");
}
