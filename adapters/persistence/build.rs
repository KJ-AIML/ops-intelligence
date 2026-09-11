//! `sqlx::migrate!("../../migrations")` embeds the directory at compile time.
//! Without this, adding a migration file does not rebuild the crate, and the
//! acceptance tests run against the previous set while reporting success.
//! Task 11 was verified against migration 6 once for exactly that reason.
fn main() {
    println!("cargo:rerun-if-changed=../../migrations");
}
