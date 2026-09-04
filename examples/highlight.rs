//! Fetches highlight.js from a CDN and prints the HTML it produces for a
//! snippet of Rust.
//!
//! Run with:  cargo run --example highlight
//!
//! Needs network access. The module loader fetches with async `reqwest`, and
//! `ContextBuilder::build` drives that fetch through Boa's job queue
//! synchronously, so we need an active Tokio runtime while it runs.
//!
//! Boa parses each imported module with a recursive-descent parser and the
//! bundled highlight.js ESM is large, so building the context can recurse
//! deeper than the default stack allows (only 1 MiB on the Windows main
//! thread). `stacker` checks the remaining stack before we start and moves the
//! work onto a fresh 64 MiB allocation if we're close to the limit, so the
//! parse can never overflow.

use hlrs::ContextBuilder;

/// Keep this much headroom before switching stacks.
const RED_ZONE: usize = 1024 * 1024;
/// Size of the replacement stack `stacker` allocates when the red zone is hit.
const NEW_STACK: usize = 64 * 1024 * 1024;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let _guard = runtime.enter();

    stacker::maybe_grow(RED_ZONE, NEW_STACK, run)
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut context = ContextBuilder::new("11.9.0").build()?;

    let snippet = r#"fn main() {
    let a = 0
    let b = 123
    std::mem::swap(&mut a, &mut b);
    println!("a: {}, b: {}", a, b);
}"#;

    let html = context.highlight("rust", snippet)?;

    println!("--- input ---\n{snippet}\n");
    println!("--- highlight.js output ---\n{html}");

    Ok(())
}
