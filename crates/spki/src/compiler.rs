//! Embedded bundling. Only ordinary ES modules cross the runtime boundary.
use anyhow::{Context, Result, ensure};
use rolldown::plugin::{
    HookLoadArgs, HookLoadOutput, HookResolveIdArgs, HookResolveIdOutput, Plugin, PluginContext,
    SharedLoadPluginContext,
};
use rolldown::{
    Bundler, BundlerOptions, BundlerTransformOptions, CodeSplittingMode, InputItem, ModuleType,
    OutputFormat, Platform, RawMinifyOptions, RawMinifyOptionsDetailed, ResolveOptions,
};
use rolldown_common::{Output, ResolvedExternal, side_effects::HookSideEffects};
use serde_json::Value;
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

type Assets = BTreeMap<String, Vec<u8>>;
type Styles = BTreeMap<String, Vec<PathBuf>>;

pub struct BuildResult {
    pub code: String,
    pub inputs: BTreeSet<PathBuf>,
}
struct BundleResult {
    output: Vec<Output>,
    assets: Assets,
    styles: Styles,
    inputs: BTreeSet<PathBuf>,
}

pub const VERSION: &str = "1.3.1";
pub struct Compiler {
    pub(crate) modules: BTreeSet<String>,
    runtime: tokio::runtime::Runtime,
}

#[derive(Debug)]
struct RuntimePlugin {
    aliases: BTreeMap<String, (String, bool)>,
    modules: BTreeSet<String>,
    browser: bool,
    assets: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
}
impl Plugin for RuntimePlugin {
    fn register_hook_usage(&self) -> rolldown::plugin::HookUsage {
        rolldown::plugin::HookUsage::ResolveId | rolldown::plugin::HookUsage::Load
    }
    fn name(&self) -> Cow<'static, str> {
        "spki-runtime".into()
    }
    async fn resolve_id(
        &self,
        _: &PluginContext,
        args: &HookResolveIdArgs<'_>,
    ) -> Result<Option<HookResolveIdOutput>> {
        let (id, external) = if let Some((id, external)) = self.aliases.get(args.specifier) {
            (id.clone(), *external)
        } else if self.modules.contains(args.specifier)
            || (self.browser
                && (args.specifier.starts_with("/capnp-es/")
                    || args.specifier.starts_with("/__sandstorm/")))
        {
            (args.specifier.to_owned(), true)
        } else {
            return Ok(None);
        };
        Ok(Some(HookResolveIdOutput {
            id: id.into(),
            external: Some(ResolvedExternal::Bool(external)),
            normalize_external_id: Some(false),
            ..Default::default()
        }))
    }
    async fn load(
        &self,
        _: SharedLoadPluginContext,
        args: &HookLoadArgs<'_>,
    ) -> Result<Option<HookLoadOutput>> {
        if !self.browser {
            return Ok(None);
        }
        let path = Path::new(args.id);
        let extension = path.extension().and_then(|s| s.to_str()).unwrap_or("");
        let code = if extension == "css" {
            // Keep CSS modules in the chunk graph; emit their styles alongside each chunk below.
            "export {};".to_owned()
        } else if matches!(
            extension,
            "png" | "jpg" | "jpeg" | "svg" | "webp" | "woff2" | "ico"
        ) {
            let url = emit_asset(path, &mut self.assets.lock().unwrap())?;
            format!("export default {};", serde_json::to_string(&url)?)
        } else {
            return Ok(None);
        };
        Ok(Some(HookLoadOutput {
            code: code.into(),
            module_type: Some(ModuleType::Js),
            side_effects: Some(HookSideEffects::NoTreeshake),
            ..Default::default()
        }))
    }
}
fn emit_asset(path: &Path, assets: &mut BTreeMap<String, Vec<u8>>) -> Result<String> {
    use sha2::{Digest, Sha256};
    let bytes = fs::read(path).with_context(|| format!("cannot read asset {}", path.display()))?;
    let hash = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let name = format!(
        "media/{}-{}.{}",
        path.file_stem().unwrap_or_default().to_string_lossy(),
        &hash[..16],
        path.extension().unwrap_or_default().to_string_lossy()
    );
    assets.insert(name.clone(), bytes);
    Ok(format!("/assets/{name}"))
}
impl Compiler {
    pub fn new() -> Result<Self> {
        let sdk: Value = serde_json::from_str(spki_common::SDK_MODULES_JSON)?;
        Ok(Self {
            modules: sdk
                .as_object()
                .context("invalid SDK module table")?
                .keys()
                .cloned()
                .collect(),
            runtime: tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?,
        })
    }
    fn bundle(
        &self,
        entries: &[PathBuf],
        cwd: &Path,
        browser: bool,
        schemas: &BTreeMap<String, PathBuf>,
    ) -> Result<BundleResult> {
        let mut aliases = BTreeMap::new();
        let prefix = if browser { "/" } else { "" };
        aliases.insert(
            "@mnutt/capnp-es".into(),
            (format!("{prefix}capnp-es/index.mjs"), true),
        );
        for name in &self.modules {
            if let Some(relative) = name.strip_prefix("capnp-es/") {
                aliases.insert(
                    format!("@mnutt/capnp-es/{}", relative.trim_end_matches(".mjs")),
                    (format!("{prefix}{name}"), true),
                );
            }
        }
        for (name, path) in schemas.iter() {
            aliases.insert(
                name.clone(),
                if browser {
                    (
                        format!(
                            "/__sandstorm/capnp/{}.js",
                            name.strip_prefix("capnp:./").unwrap()
                        ),
                        true,
                    )
                } else {
                    (path.to_string_lossy().into_owned(), false)
                },
            );
        }
        let assets = Arc::new(Mutex::new(BTreeMap::new()));
        let plugin = RuntimePlugin {
            aliases,
            modules: self.modules.clone(),
            browser,
            assets: assets.clone(),
        };
        let options = BundlerOptions {
            input: Some(
                entries
                    .iter()
                    .map(|path| InputItem {
                        name: Some(
                            path.file_stem()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .into_owned(),
                        ),
                        import: path.to_string_lossy().into_owned(),
                    })
                    .collect(),
            ),
            cwd: Some(cwd.to_owned()),
            platform: Some(if browser {
                Platform::Browser
            } else {
                Platform::Neutral
            }),
            format: Some(OutputFormat::Esm),
            polyfill_require: Some(false),
            code_splitting: Some(CodeSplittingMode::Bool(browser)),
            entry_filenames: Some("[name].js".to_owned().into()),
            chunk_filenames: Some("chunks/[name]-[hash].js".to_owned().into()),
            module_types: Some([(".txt".into(), ModuleType::Text)].into_iter().collect()),
            resolve: Some(ResolveOptions {
                condition_names: Some(if browser {
                    vec!["browser".into()]
                } else {
                    vec!["workerd".into(), "worker".into(), "browser".into()]
                }),
                main_fields: Some(if browser {
                    vec!["browser".into(), "module".into(), "main".into()]
                } else {
                    vec!["module".into(), "main".into()]
                }),
                ..Default::default()
            }),
            transform: Some(BundlerTransformOptions {
                target: Some(rolldown::Either::Left("es2022".into())),
                ..Default::default()
            }),
            // Remove generated source-path banners without renaming identifiers or compressing semantics.
            minify: Some(RawMinifyOptions::Object(RawMinifyOptionsDetailed {
                mangle: None,
                mangle_properties: None,
                compress: None,
                remove_whitespace: true,
                ascii_only: false,
            })),
            ..Default::default()
        };
        let (result, styles, inputs) = self.runtime.block_on(async {
            let mut bundler =
                Bundler::with_plugins(options, vec![RuntimePlugin::new_shared(plugin)])
                    .map_err(|e| anyhow::anyhow!("{e:?}"))?;
            let result = bundler
                .generate()
                .await
                .map_err(|e| anyhow::anyhow!("bundle failed: {e:?}"))?;
            // Unresolved imports must never silently turn into runtime dependencies.
            for warning in &result.warnings {
                ensure!(
                    warning.kind().to_string() != "UNRESOLVED_IMPORT",
                    "bundle failed: {warning:?}"
                );
                eprintln!("bundle warning: {warning}");
            }
            let infos = bundler.module_infos();
            let mut inputs: BTreeSet<PathBuf> = bundler
                .watch_files()
                .iter()
                .map(|s| PathBuf::from(s.as_str()))
                .collect();
            inputs.extend(
                infos
                    .iter()
                    .map(|info| PathBuf::from(info.key().as_str()))
                    .filter(|p| p.is_file()),
            );
            let mut styles = BTreeMap::new();
            for item in &result.assets {
                if let Output::Chunk(chunk) = item {
                    if chunk.is_entry {
                        let mut visited = BTreeSet::new();
                        let mut paths = Vec::new();
                        fn visit(
                            id: &rolldown_common::ModuleId,
                            infos: &rolldown_common::SharedModuleInfoDashMap,
                            visited: &mut BTreeSet<String>,
                            paths: &mut Vec<PathBuf>,
                        ) {
                            if !visited.insert(id.to_string()) {
                                return;
                            }
                            if let Some(info) = infos.get(id.as_str()) {
                                let imports: Vec<_> = info
                                    .imported_ids
                                    .iter()
                                    .chain(&info.dynamically_imported_ids)
                                    .cloned()
                                    .collect();
                                drop(info);
                                for imported in imports {
                                    visit(&imported, infos, visited, paths);
                                }
                            }
                            let path = PathBuf::from(id.as_str());
                            if path.extension().is_some_and(|ext| ext == "css") {
                                paths.push(path);
                            }
                        }
                        if let Some(id) = &chunk.facade_module_id {
                            visit(id, &infos, &mut visited, &mut paths);
                        }
                        styles.insert(chunk.filename.to_string(), paths);
                    }
                }
            }
            Ok::<_, anyhow::Error>((result.assets, styles, inputs))
        })?;
        let emitted = assets.lock().unwrap().clone();
        Ok(BundleResult {
            output: result,
            assets: emitted,
            styles,
            inputs,
        })
    }
    #[cfg(test)]
    pub fn build(&self, entry: &Path, limit: usize) -> Result<BuildResult> {
        self.build_worker(entry, limit, &BTreeMap::new())
    }
    pub(crate) fn build_worker(
        &self,
        entry: &Path,
        limit: usize,
        schemas: &BTreeMap<String, PathBuf>,
    ) -> Result<BuildResult> {
        let entry = fs::canonicalize(entry).context("cannot read worker entrypoint")?;
        let BundleResult { output, inputs, .. } = self.bundle(
            std::slice::from_ref(&entry),
            entry.parent().unwrap(),
            false,
            schemas,
        )?;
        ensure!(
            output.len() == 1,
            "worker must compile to one ES module; separate CSS/assets are not supported yet"
        );
        let Output::Chunk(chunk) = &output[0] else {
            anyhow::bail!("worker must compile to one ES module")
        };
        for import in chunk.imports.iter().chain(&chunk.dynamic_imports) {
            ensure!(
                self.modules.contains(import.as_str()),
                "bundle contains an unsupported external import: {import}"
            );
        }
        validate_worker(&chunk.code)?;
        ensure!(chunk.code.len() <= limit, "source exceeds Preview limit");
        Ok(BuildResult {
            code: chunk.code.clone(),
            inputs,
        })
    }
    pub(crate) fn build_browser(
        &self,
        entries: &[PathBuf],
        cwd: &Path,
        output: &Path,
        schemas: &BTreeMap<String, PathBuf>,
    ) -> Result<BTreeSet<PathBuf>> {
        let BundleResult {
            output: chunks,
            mut assets,
            styles,
            mut inputs,
        } = self.bundle(entries, cwd, true, schemas)?;
        for item in &chunks {
            let path = output.join(item.filename());
            fs::create_dir_all(path.parent().unwrap())?;
            fs::write(path, item.content_as_bytes())?;
            if let Output::Chunk(chunk) = item {
                let mut css = String::new();
                if let Some(paths) = styles
                    .get(chunk.filename.as_str())
                    .filter(|paths| !paths.is_empty())
                {
                    css = self.compile_css(paths, &mut assets, &mut inputs)?;
                }
                if !css.is_empty() {
                    fs::write(
                        output.join(chunk.filename.as_str()).with_extension("css"),
                        css,
                    )?;
                }
            }
        }
        for (name, bytes) in assets {
            let path = output.join(name);
            fs::create_dir_all(path.parent().unwrap())?;
            fs::write(path, bytes)?;
        }
        Ok(inputs)
    }
    fn compile_css(
        &self,
        paths: &[PathBuf],
        assets: &mut BTreeMap<String, Vec<u8>>,
        inputs: &mut BTreeSet<PathBuf>,
    ) -> Result<String> {
        use lightningcss::{
            bundler::{Bundler as CssBundler, FileProvider},
            dependencies::{Dependency, DependencyOptions},
            stylesheet::{ParserOptions, PrinterOptions},
        };
        // Bundle one synthetic stylesheet per entry so @import deduplication and cascade
        // ordering apply across all JS imports, including shared and lazy chunks.
        let stage = tempfile::tempdir()?;
        let entry = stage.path().join("entry.css");
        let imports = paths
            .iter()
            .map(|path| {
                Ok(format!(
                    "@import {};\n",
                    serde_json::to_string(&path.to_string_lossy())?
                ))
            })
            .collect::<Result<String>>()?;
        fs::write(&entry, imports)?;
        let provider = FileProvider::new();
        let mut bundler = CssBundler::new(&provider, None, ParserOptions::default());
        let sheet = bundler
            .bundle(&entry)
            .map_err(|e| anyhow::anyhow!("CSS bundle failed: {e}"))?;
        inputs.extend(
            sheet
                .sources
                .iter()
                .map(PathBuf::from)
                .filter(|p| !p.starts_with(stage.path())),
        );
        let result = sheet
            .to_css(PrinterOptions {
                analyze_dependencies: Some(DependencyOptions::default()),
                ..Default::default()
            })
            .map_err(|e| anyhow::anyhow!("CSS output failed: {e}"))?;
        let mut code = result.code;
        for dependency in result.dependencies.unwrap_or_default() {
            let (url, placeholder, location) = match dependency {
                Dependency::Url(d) => (d.url, d.placeholder, d.loc),
                Dependency::Import(d) => (d.url, d.placeholder, d.loc),
            };
            let replacement = if url.starts_with('/') || url.starts_with('#') || url.contains(':') {
                url
            } else {
                let (file, suffix) = url
                    .find(['?', '#'])
                    .map(|i| (&url[..i], &url[i..]))
                    .unwrap_or((&url, ""));
                let asset = Path::new(&location.file_path).parent().unwrap().join(file);
                inputs.insert(asset.clone());
                format!("{}{suffix}", emit_asset(&asset, assets)?)
            };
            code = code.replace(&placeholder, &replacement);
        }
        Ok(code)
    }
}
fn validate_worker(code: &str) -> Result<()> {
    use oxc_ast_visit::{Visit, walk};
    #[derive(Default)]
    struct Check {
        dynamic: bool,
    }
    impl<'a> Visit<'a> for Check {
        fn visit_import_expression(&mut self, it: &oxc_ast::ast::ImportExpression<'a>) {
            self.dynamic |= !matches!(it.source, oxc_ast::ast::Expression::StringLiteral(_));
            walk::walk_import_expression(self, it);
        }
    }
    let allocator = oxc_allocator::Allocator::default();
    let parsed = oxc_parser::Parser::new(&allocator, code, oxc_span::SourceType::mjs()).parse();
    ensure!(
        parsed.diagnostics.is_empty(),
        "invalid generated JavaScript"
    );
    let semantic = oxc_semantic::SemanticBuilder::new()
        .build(&parsed.program)
        .semantic;
    ensure!(
        !semantic
            .scoping()
            .root_unresolved_references()
            .contains_key("require"),
        "worker contains an unsupported runtime require"
    );
    let mut check = Check::default();
    check.visit_program(&parsed.program);
    ensure!(
        !check.dynamic,
        "worker contains an unsupported dynamic import or require call"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_external_sources_and_transitive_css_assets() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("project");
        fs::create_dir(&root).unwrap();
        let helper = temp.path().canonicalize().unwrap().join("helper.ts");
        fs::write(&helper, "export const value: number = 42;").unwrap();
        let worker = root.join("worker.ts");
        fs::write(&worker, "export {value} from '../helper';").unwrap();
        let compiler = Compiler::new().unwrap();
        let worker_build = compiler.build(&worker, 8192).unwrap();
        assert!(worker_build.inputs.contains(&helper));
        let entry = root.join("main.ts");
        fs::write(&entry, "import './style.css'; console.log('loaded');").unwrap();
        fs::write(root.join("style.css"), "@import '../shared.css';").unwrap();
        let shared = temp.path().join("shared.css");
        let image = temp.path().join("logo.svg");
        fs::write(&shared, "body { background: url('./logo.svg'); }").unwrap();
        fs::write(&image, "<svg/>").unwrap();
        let inputs = compiler
            .build_browser(&[entry], &root, &root.join("out"), &BTreeMap::new())
            .unwrap();
        assert!(
            worker_build.inputs.contains(&helper),
            "browser build must not overwrite worker inputs"
        );
        assert!(
            !inputs.contains(&helper),
            "browser inputs must not contain prior worker inputs"
        );
        // CSS imports may retain .. segments, so compare canonical paths.
        let inputs: BTreeSet<_> = inputs
            .iter()
            .filter_map(|p| p.canonicalize().ok())
            .collect();
        assert!(inputs.contains(&shared.canonicalize().unwrap()));
        assert!(inputs.contains(&image.canonicalize().unwrap()));
        assert!(inputs.contains(&root.join("style.css").canonicalize().unwrap()));
    }

    #[test]
    fn runtime_require_validation_respects_local_bindings() {
        assert!(validate_worker("export default () => require(globalThis.name);").is_err());
        assert!(
            validate_worker("export default function(require) { return require('local'); }")
                .is_ok()
        );
    }
}
