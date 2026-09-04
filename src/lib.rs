use boa::builtins::promise::PromiseState;
use boa::module::{Module, ModuleLoader, ModuleRequest, Referrer};
use boa::{JsObject, js_string};
use boa_engine as boa;
use std::{cell::RefCell, future::Future, rc::Rc};

/// Sometimes things just go wrong :(
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The Boa engine could not be constructed.
    #[error("failed to initialise the JavaScript engine: {0}")]
    EngineInit(boa::JsError),

    /// The imported module could not be parsed.
    /// Usually means an `install` name is not a valid identifier.
    #[error("failed to parse the generated import module: {0}")]
    ModuleParse(boa::JsError),

    /// Running the pending job queue (module loading, promises) failed.
    #[error("failed to drain the JavaScript job queue: {0}")]
    JobQueue(boa::JsError),

    /// An imported module could not be loaded, linked or evaluated. Wraps the
    /// message produced by the rejected module promise (network failure,
    /// unknown protocol, syntax error in the fetched source, ...).
    #[error("failed to load an imported module: {0}")]
    ModuleLoad(String),

    /// Module loading never settled. Should not happen for the loaders bundled
    /// here, but callers get an error rather than a hang or a panic.
    #[error("module loading did not complete")]
    ModulePending,

    /// Setting up the `config` object handed to `hljs.highlight` failed.
    #[error("failed to set up the highlight configuration: {0}")]
    Config(boa::JsError),

    /// `hljs.highlight(...)` threw. Most often the `hljs` import is missing or
    /// the requested language is not registered.
    #[error("failed to execute highlight.js: {0}")]
    Execution(boa::JsError),

    /// `hljs.highlight(...).value` was not a string.
    #[error("highlight.js did not return a string result")]
    NonStringResult,
}

fn describe_rejection(value: boa::JsValue, context: &mut boa::Context) -> String {
    value
        .to_string(context)
        .map(|s| s.to_std_string_lossy())
        .unwrap_or_else(|_| "<unrepresentable error value>".to_owned())
}

#[derive(Debug, Default)]
pub struct HybridModuleLoader;

impl ModuleLoader for HybridModuleLoader {
    // The trait method now consumes self as Rc<Self>, takes a ModuleRequest,
    // accepts a RefCell Context tracker, and returns an async Future.
    fn load_imported_module(
        self: Rc<Self>,
        _referrer: Referrer,
        request: ModuleRequest,
        context: &RefCell<&mut boa::Context>,
    ) -> impl Future<Output = boa::JsResult<Module>> {
        let context_cell = context;
        let specifier = request.specifier().to_std_string();

        async move {
            let bytes = match specifier {
                Ok(specifier)
                    if specifier.starts_with("http://") || specifier.starts_with("https://") =>
                {
                    // We use standard async reqwest instead of blocking
                    let response = reqwest::get(&specifier).await.map_err(|err| {
                        boa::JsNativeError::error()
                            .with_message(format!("Network fetch failed: {err}"))
                    })?;

                    let js_code = response.text().await.map_err(|err| {
                        boa::JsNativeError::error()
                            .with_message(format!("Failed to extract response body: {err}"))
                    })?;

                    js_code.into_bytes()
                }
                Ok(specifier) => {
                    return Err(boa::JsNativeError::error()
                        .with_message(format!(
                            "Unsupported module protocol specifier: {specifier}"
                        ))
                        .into());
                }

                Err(err) => {
                    return Err(boa::JsNativeError::error()
                        .with_message(format!("Failed to decode module specifier: {err}"))
                        .into());
                }
            };

            let source = boa::Source::from_bytes(&bytes);

            let mut ctx_borrow = context_cell.borrow_mut();
            Module::parse(source, None, &mut ctx_borrow)
        }
    }
}

#[derive(Debug)]
pub struct ContextBuilder {
    includes: Vec<(Option<String>, String)>,
}

impl ContextBuilder {
    /// Same as new but fetches the latest version
    pub fn latest() -> Self {
        Self::new("latest")
    }

    /// Creates a ContextBuilder and pulls the ESM bundle from jsDelivr: it has
    /// every common language with no further imports
    pub fn new(version: &str) -> Self {
        let mut obj = ContextBuilder {
            includes: Vec::new(),
        };

        let url = format!(
            "https://cdn.jsdelivr.net/npm/@highlightjs/cdn-assets@{version}/es/highlight.min.js"
        );

        obj.includes.push((Some("hljs".into()), url));
        obj
    }
}

impl ContextBuilder {
    /// Register a module to import from `url`.
    pub fn install(mut self, url: impl Into<String>) -> Self {
        self.includes.push((None, url.into()));
        self
    }

    /// Build a [`Context`], loading and evaluating every configured import.
    ///
    /// # Stack usage
    ///
    /// Boa parses each imported module with a recursive descent parser, so a
    /// large bundle (the default highlight.js ESM is ~120 KiB minified) can need
    /// more stack than the platform default, in particular the 1 MiB main
    /// thread stack on Windows. If you hit a stack overflow, run this on a
    /// thread created with an enlarged
    /// [`stack_size`](std::thread::Builder::stack_size), or grow the stack on
    /// demand with the `stacker` crate as the `highlight` example does.
    pub fn build(self) -> Result<Context, Error> {
        let mut context = boa::Context::builder()
            .module_loader(Rc::new(HybridModuleLoader))
            .build()
            .map_err(Error::EngineInit)?;

        if self.includes.is_empty() {
            return Ok(Context { context });
        }

        let mut imports = String::new();
        for (name, url) in &self.includes {
            let statement = if let Some(name) = name {
                format!("import {name} from {url:?};globalThis.{name} = {name};")
            } else {
                format!("import {url:?};")
            };

            imports.push_str(&statement);
        }

        let source = boa::Source::from_bytes(imports.as_bytes());
        let module = Module::parse(source, None, &mut context).map_err(Error::ModuleParse)?;

        let promise = module.load_link_evaluate(&mut context);
        context.run_jobs().map_err(Error::JobQueue)?;

        match promise.state() {
            PromiseState::Fulfilled(_) => Ok(Context { context }),
            PromiseState::Pending => Err(Error::ModulePending),
            PromiseState::Rejected(value) => {
                Err(Error::ModuleLoad(describe_rejection(value, &mut context)))
            }
        }
    }
}

pub struct Context {
    context: boa::Context,
}

impl Context {
    /// Highlight `code` as `language` using the installed `hljs` module,
    /// returning the produced HTML string.
    pub fn highlight(
        &mut self,
        language: impl Into<String>,
        code: impl Into<String>,
    ) -> Result<String, Error> {
        let context = &mut self.context;
        let language = language.into();
        let code = code.into();

        let config = JsObject::with_object_proto(context.intrinsics());
        config
            .set(js_string!("language"), js_string!(language), true, context)
            .map_err(Error::Config)?;
        config
            .set(js_string!("__code"), js_string!(code), true, context)
            .map_err(Error::Config)?;

        context
            .global_object()
            .set(js_string!("config"), config, false, context)
            .map_err(Error::Config)?;

        let js_script = r"hljs.highlight(globalThis.config.__code, { language: globalThis.config.language }).value";
        let source = boa::Source::from_bytes(js_script.as_bytes());

        let result = context.eval(source).map_err(Error::Execution)?;

        result
            .as_string()
            .map(|js_str| js_str.to_std_string_lossy())
            .ok_or(Error::NonStringResult)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A builder with no configured imports. `ContextBuilder::new`/`latest`
    /// always seed the hljs bundle, so tests that need an empty graph build
    /// one directly.
    fn empty_builder() -> ContextBuilder {
        ContextBuilder {
            includes: Vec::new(),
        }
    }

    #[test]
    fn new_pins_the_hljs_bundle_to_the_requested_version() {
        let builder = ContextBuilder::new("11.9.0");

        assert_eq!(
            builder.includes,
            vec![(
                Some("hljs".to_owned()),
                "https://cdn.jsdelivr.net/npm/@highlightjs/cdn-assets@11.9.0/es/highlight.min.js"
                    .to_owned(),
            )]
        );
    }

    #[test]
    fn latest_is_new_with_the_latest_tag() {
        assert_eq!(
            ContextBuilder::latest().includes,
            ContextBuilder::new("latest").includes
        );
    }

    #[test]
    fn install_appends_an_unnamed_side_effect_import() {
        let builder = ContextBuilder::latest().install("https://example.com/rust.min.js");

        assert_eq!(
            builder.includes.last(),
            Some(&(None, "https://example.com/rust.min.js".to_owned()))
        );
    }

    #[test]
    fn install_is_chainable_and_keeps_every_entry_in_order() {
        let builder = empty_builder()
            .install("https://example.com/a.js")
            .install("https://example.com/b.js");

        assert_eq!(
            builder.includes,
            vec![
                (None, "https://example.com/a.js".to_owned()),
                (None, "https://example.com/b.js".to_owned()),
            ]
        );
    }

    #[test]
    fn install_accepts_both_str_and_string() {
        let owned = String::from("https://example.com/owned.js");
        let builder = empty_builder()
            .install("https://example.com/borrowed.js")
            .install(owned);

        assert_eq!(builder.includes.len(), 2);
    }

    #[test]
    fn build_without_imports_succeeds() {
        assert!(empty_builder().build().is_ok());
    }

    #[test]
    fn build_reports_unsupported_protocol_instead_of_panicking() {
        let Err(err) = empty_builder().install("ftp://example.com/mod.js").build() else {
            panic!("ftp is not a supported protocol");
        };

        match err {
            Error::ModuleLoad(msg) => assert!(
                msg.contains("Unsupported module protocol"),
                "unexpected message: {msg}"
            ),
            other => panic!("expected Error::ModuleLoad, got {other:?}"),
        }
    }

    #[test]
    fn build_reports_invalid_import_name_instead_of_panicking() {
        // Fails while parsing the synthesised import module, before any fetch.
        let builder = ContextBuilder {
            includes: vec![(
                Some("not an identifier".to_owned()),
                "https://example.com/hljs.js".to_owned(),
            )],
        };

        let Err(err) = builder.build() else {
            panic!("an invalid JS identifier should not parse");
        };

        assert!(matches!(err, Error::ModuleParse(_)));
    }

    #[test]
    fn highlight_without_hljs_returns_execution_error() {
        let mut context = empty_builder().build().expect("empty context builds");

        let err = context
            .highlight("rust", "let x = 1;")
            .expect_err("no hljs was installed");

        assert!(matches!(err, Error::Execution(_)));
    }

    #[test]
    fn highlight_accepts_str_and_string_arguments() {
        let mut context = empty_builder().build().expect("empty context builds");

        // Still fails (no hljs), but exercises the `impl Into<String>` bounds
        // for both a borrowed and an owned argument.
        let err = context
            .highlight(String::from("rust"), "let x = 1;")
            .expect_err("no hljs was installed");

        assert!(matches!(err, Error::Execution(_)));
    }

    #[test]
    fn error_display_is_human_readable() {
        assert_eq!(
            Error::ModulePending.to_string(),
            "module loading did not complete"
        );
        assert_eq!(
            Error::NonStringResult.to_string(),
            "highlight.js did not return a string result"
        );
        assert_eq!(
            Error::ModuleLoad("boom".to_owned()).to_string(),
            "failed to load an imported module: boom"
        );
    }
}
