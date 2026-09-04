# hlrs

Syntax highlighting for Rust, using [highlight.js](https://highlightjs.org/)
running inside a tiny [Boa](https://github.com/boa-dev/boa) runtime.

`hlrs` builds a small JavaScript context, imports the highlight.js ESM bundle
from a CDN, and calls `hljs.highlight` for you. The result is the same HTML that
highlight.js produces in the browser.

## Installation

```toml
[dependencies]
hlrs = "0.1"
```

or 

```shell
$ cargo install hlrs
```

Building a context fetches modules over the network with async `reqwest`, so a
Tokio runtime must be active while `ContextBuilder::build` runs.

## Usage

```rust
use hlrs::ContextBuilder;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let _guard = runtime.enter();

    // Pull a pinned highlight.js bundle. Use `ContextBuilder::latest()` for the
    // newest release.
    let mut context = ContextBuilder::new("11.9.0").build()?;

    let code = "fn main() { println!(\"hi\"); }";
    let html = context.highlight("rust", code)?;

    println!("{html}");
    Ok(())
}
```

### Extra modules

`install` registers an additional module to import before highlighting, which is
useful for language packs or plugins that attach themselves to the global
`hljs`:

```rust
let context = ContextBuilder::new("11.9.0")
    .install("https://cdn.jsdelivr.net/npm/some-hljs-plugin/es/plugin.min.js")
    .build()?;
```



## License

GNU Lesser General Public License v3.0 or later (`LGPL-3.0-or-later`). See
[LICENSE](LICENSE).