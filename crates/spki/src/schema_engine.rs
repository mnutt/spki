//! The pinned capnp-es generator runs in-process; Oxc emits its TypeScript as JS.
use anyhow::{Context, Result, ensure};
use rquickjs::{CatchResultExt, Function, TypedArray};
use std::{
    path::Path,
    time::{Duration, Instant},
};

const GENERATOR: &str = include_str!("../schema-compiler/compiler.js");
const TEXT_CODECS: &str = r#"
globalThis.TextEncoder = class {
  encode(text = '') { return new Uint8Array(__encodeUtf8(String(text))); }
};
globalThis.TextDecoder = class {
  decode(value = new Uint8Array()) {
    const bytes = ArrayBuffer.isView(value)
      ? new Uint8Array(value.buffer, value.byteOffset, value.byteLength)
      : new Uint8Array(value);
    return __decodeUtf8(Array.from(bytes));
  }
};
"#;

pub(crate) fn generate(request: &[u8]) -> Result<Vec<(String, String)>> {
    let runtime = rquickjs::Runtime::new().context("create schema generator runtime")?;
    runtime.set_memory_limit(256 * 1024 * 1024);
    runtime.set_max_stack_size(1024 * 1024);
    let deadline = Instant::now() + Duration::from_secs(30);
    runtime.set_interrupt_handler(Some(Box::new(move || Instant::now() >= deadline)));
    let context = rquickjs::Context::full(&runtime).context("create schema generator context")?;
    let output = context.with(|ctx| -> Result<String> {
        let run = || -> rquickjs::Result<String> {
            let globals = ctx.globals();
            globals.set(
                "__encodeUtf8",
                Function::new(ctx.clone(), |value: String| value.into_bytes())?,
            )?;
            globals.set(
                "__decodeUtf8",
                Function::new(ctx.clone(), |value: Vec<u8>| {
                    let text = String::from_utf8_lossy(&value);
                    // Match the default UTF-8 TextDecoder's BOM handling.
                    text.strip_prefix('\u{feff}').unwrap_or(&text).to_owned()
                })?,
            )?;
            ctx.eval::<(), _>(TEXT_CODECS)?;
            ctx.eval::<(), _>(GENERATOR)?;
            let generate: Function = globals.get("__spkiGenerate")?;
            generate.call((TypedArray::<u8>::new_copy(ctx.clone(), request)?,))
        };
        run()
            .catch(&ctx)
            .map_err(|e| anyhow::anyhow!("capnp-es generation failed: {e}"))
    })?;
    let files: Vec<(String, String)> =
        serde_json::from_str(&output).context("invalid generated schema source map")?;
    files
        .into_iter()
        .map(|(name, source)| {
            let stem = name
                .strip_suffix(".ts")
                .context("generator emitted a non-TypeScript file")?;
            ensure!(
                Path::new(stem)
                    .components()
                    .all(|p| matches!(p, std::path::Component::Normal(_))),
                "invalid generated schema path"
            );
            Ok((format!("{stem}.js"), transpile(&name, &source)?))
        })
        .collect()
}

fn transpile(name: &str, source: &str) -> Result<String> {
    let allocator = oxc_allocator::Allocator::default();
    let parsed = oxc_parser::Parser::new(&allocator, source, oxc_span::SourceType::ts()).parse();
    ensure!(
        parsed.diagnostics.is_empty(),
        "invalid generated TypeScript in {name}: {:?}",
        parsed.diagnostics
    );
    let mut program = parsed.program;
    let semantic = oxc_semantic::SemanticBuilder::new()
        .with_excess_capacity(2.0)
        .with_enum_eval(true)
        .build(&program);
    ensure!(
        semantic.diagnostics.is_empty(),
        "invalid generated bindings in {name}: {:?}",
        semantic.diagnostics
    );
    let options =
        oxc_transformer::TransformOptions::from_target("es2022").map_err(anyhow::Error::msg)?;
    let transformed = oxc_transformer::Transformer::new(&allocator, Path::new(name), &options)
        .build_with_scoping(semantic.semantic.into_scoping(), &mut program);
    ensure!(
        transformed.diagnostics.is_empty(),
        "schema transformation failed in {name}: {:?}",
        transformed.diagnostics
    );
    Ok(oxc_codegen::Codegen::new().build(&program).code)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_malformed_compiler_requests_without_panicking() {
        assert!(generate(&[0, 1, 2]).is_err());
    }
    #[test]
    fn empty_request_needs_no_host_tools() {
        let mut message = capnp::message::Builder::new_default();
        message.init_root::<capnp::schema_capnp::code_generator_request::Builder>();
        let bytes = capnp::serialize::write_message_to_words(&message);
        assert!(generate(&bytes).unwrap().is_empty());
    }
}
