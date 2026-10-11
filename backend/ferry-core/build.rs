fn main() {
    // sqlx::migrate! embeds the files it finds at compile time; rebuild when one is added.
    println!("cargo:rerun-if-changed=../migrations");
}
