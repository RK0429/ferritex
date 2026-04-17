use std::collections::{HashMap, HashSet};

use super::{EnvironmentDef, MacroDef, MacroEngine, Token, TokenKind};

/// Metadata for a loaded package
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageInfo {
    pub name: String,
    pub options: Vec<String>,
}

/// Metadata for the active document class
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassInfo {
    pub name: String,
    pub options: Vec<String>,
}

/// Registry tracking loaded packages with duplicate-load protection
#[derive(Debug, Clone, Default)]
pub struct PackageRegistry {
    packages: Vec<PackageInfo>,
}

impl PackageRegistry {
    pub fn is_loaded(&self, name: &str) -> bool {
        self.packages.iter().any(|package| package.name == name)
    }

    pub fn load(&mut self, info: PackageInfo) -> bool {
        if self.is_loaded(&info.name) {
            return false;
        }

        self.packages.push(info);
        true
    }

    pub fn loaded_packages(&self) -> &[PackageInfo] {
        &self.packages
    }

    fn unload(&mut self, name: &str) {
        self.packages.retain(|package| package.name != name);
    }
}

/// Registry tracking active document class
#[derive(Debug, Clone, Default)]
pub struct ClassRegistry {
    active_class: Option<ClassInfo>,
}

impl ClassRegistry {
    pub fn set_class(&mut self, info: ClassInfo) {
        self.active_class = Some(info);
    }

    pub fn active_class(&self) -> Option<&ClassInfo> {
        self.active_class.as_ref()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OptionRegistry {
    pub options: HashMap<String, Vec<Token>>,
    pub default_handler: Option<Vec<Token>>,
    declaration_order: Vec<String>,
}

impl OptionRegistry {
    pub fn clear(&mut self) {
        self.options.clear();
        self.default_handler = None;
        self.declaration_order.clear();
    }

    pub fn declare_option(&mut self, name: String, code: Vec<Token>) {
        if !self.options.contains_key(&name) {
            self.declaration_order.push(name.clone());
        }
        let _ = self.options.insert(name, code);
    }

    pub fn declare_default(&mut self, code: Vec<Token>) {
        self.default_handler = Some(code);
    }

    pub fn process_options(&self, options: &[String]) -> Vec<Token> {
        let mut handled = HashSet::new();
        let mut expanded = Vec::new();

        for name in &self.declaration_order {
            if options.iter().any(|option| option == name) {
                handled.insert(name.clone());
                if let Some(code) = self.options.get(name) {
                    expanded.extend(code.clone());
                }
            }
        }

        for option in options {
            if handled.contains(option) {
                continue;
            }
            if let Some(default_handler) = &self.default_handler {
                expanded.extend(default_handler.clone());
            }
        }

        expanded
    }

    pub fn execute_options(&self, options: &[String]) -> Vec<Token> {
        let mut expanded = Vec::new();

        for option in options {
            if let Some(code) = self.options.get(option) {
                expanded.extend(code.clone());
            } else if let Some(default_handler) = &self.default_handler {
                expanded.extend(default_handler.clone());
            }
        }

        expanded
    }
}

/// Trait for native package extensions that register commands/environments
pub trait PackageExtension {
    fn name(&self) -> &str;
    fn register(&self, engine: &mut MacroEngine);
}

pub type StyPackageResolver<'a> = dyn Fn(&str) -> Option<String> + 'a;

pub const SUPPORTED_CLASSES: &[&str] = &["article", "report", "book", "letter"];

#[derive(Debug, Clone, Copy, Default)]
pub struct AmsmathExtension;

#[derive(Debug, Clone, Copy, Default)]
pub struct GraphicxExtension;

#[derive(Debug, Clone, Copy, Default)]
pub struct XcolorExtension;

#[derive(Debug, Clone, Copy, Default)]
pub struct GeometryExtension;

#[derive(Debug, Clone, Copy, Default)]
pub struct FontspecExtension;

#[derive(Debug, Clone, Copy, Default)]
pub struct MulticolExtension;

pub struct StyInterpreter<'a, 'resolver> {
    source: &'a str,
    options: &'a [String],
    registry: &'a mut PackageRegistry,
    engine: &'a mut MacroEngine,
    active_class: Option<ClassInfo>,
    sty_resolver: Option<&'resolver StyPackageResolver<'resolver>>,
}

impl<'a, 'resolver> StyInterpreter<'a, 'resolver> {
    pub fn new(
        source: &'a str,
        options: &'a [String],
        registry: &'a mut PackageRegistry,
        engine: &'a mut MacroEngine,
        active_class: Option<ClassInfo>,
        sty_resolver: Option<&'resolver StyPackageResolver<'resolver>>,
    ) -> Self {
        Self {
            source,
            options,
            registry,
            engine,
            active_class,
            sty_resolver,
        }
    }

    pub fn interpret(self) -> Result<(), String> {
        super::api::interpret_sty_package_source(
            self.source,
            self.options,
            self.registry,
            self.engine,
            self.active_class,
            self.sty_resolver,
        )
    }
}

impl PackageExtension for AmsmathExtension {
    fn name(&self) -> &str {
        "amsmath"
    }

    fn register(&self, engine: &mut MacroEngine) {
        for name in [
            "align",
            "align*",
            "alignat",
            "alignat*",
            "equation*",
            "flalign",
            "flalign*",
            "gather",
            "gather*",
            "multline",
            "multline*",
            "split",
        ] {
            register_transparent_environment(engine, name);
        }
    }
}

impl PackageExtension for GraphicxExtension {
    fn name(&self) -> &str {
        "graphicx"
    }

    fn register(&self, engine: &mut MacroEngine) {
        register_noop_command(engine, "includegraphics", 1);
    }
}

impl PackageExtension for XcolorExtension {
    fn name(&self) -> &str {
        "xcolor"
    }

    fn register(&self, engine: &mut MacroEngine) {
        register_noop_command(engine, "color", 1);
        register_passthrough_command(engine, "textcolor", 2, 2);
        register_noop_command(engine, "definecolor", 3);
    }
}

impl PackageExtension for GeometryExtension {
    fn name(&self) -> &str {
        "geometry"
    }

    fn register(&self, engine: &mut MacroEngine) {
        register_noop_command(engine, "geometry", 1);
    }
}

impl PackageExtension for FontspecExtension {
    fn name(&self) -> &str {
        "fontspec"
    }

    fn register(&self, engine: &mut MacroEngine) {
        register_noop_command(engine, "setmainfont", 1);
        register_noop_command(engine, "setsansfont", 1);
        register_noop_command(engine, "setmonofont", 1);
    }
}

impl PackageExtension for MulticolExtension {
    fn name(&self) -> &str {
        "multicol"
    }

    fn register(&self, _engine: &mut MacroEngine) {
        // multicols environment and \columnbreak are handled directly by the parser
    }
}

pub fn load_document_class(
    name: &str,
    options: &[String],
    class_registry: &mut ClassRegistry,
    engine: &mut MacroEngine,
) -> Result<(), String> {
    match name {
        "article" => {
            class_registry.set_class(ClassInfo {
                name: name.to_string(),
                options: options.to_vec(),
            });
            register_base_latex_commands(engine);
            register_noop_command(engine, "section", 1);
            register_noop_command(engine, "subsection", 1);
            register_noop_command(engine, "subsubsection", 1);
            Ok(())
        }
        "report" => {
            class_registry.set_class(ClassInfo {
                name: name.to_string(),
                options: options.to_vec(),
            });
            register_base_latex_commands(engine);
            register_noop_command(engine, "chapter", 1);
            register_noop_command(engine, "section", 1);
            register_noop_command(engine, "subsection", 1);
            register_noop_command(engine, "subsubsection", 1);
            Ok(())
        }
        "book" => {
            class_registry.set_class(ClassInfo {
                name: name.to_string(),
                options: options.to_vec(),
            });
            register_base_latex_commands(engine);
            register_noop_command(engine, "chapter", 1);
            register_noop_command(engine, "section", 1);
            register_noop_command(engine, "subsection", 1);
            register_noop_command(engine, "subsubsection", 1);
            register_alias_command(engine, "frontmatter", "cleardoublepage");
            register_alias_command(engine, "mainmatter", "cleardoublepage");
            register_alias_command(engine, "backmatter", "cleardoublepage");
            Ok(())
        }
        "letter" => {
            class_registry.set_class(ClassInfo {
                name: name.to_string(),
                options: options.to_vec(),
            });
            register_base_latex_commands(engine);
            register_passthrough_command(engine, "opening", 1, 1);
            register_passthrough_command(engine, "closing", 1, 1);
            Ok(())
        }
        _ => Err(format!("Unknown document class: {name}")),
    }
}

pub fn register_base_latex_commands(engine: &mut MacroEngine) {
    for name in [
        "abstract",
        "center",
        "description",
        "document",
        "enumerate",
        "equation",
        "figure",
        "flushleft",
        "flushright",
        "itemize",
        "quote",
        "quotation",
        "table",
        "tabular",
        "thebibliography",
        "verse",
    ] {
        register_transparent_environment(engine, name);
    }
    register_minipage_environment(engine);

    register_noop_command(engine, "author", 1);
    register_noop_command(engine, "@gobble", 1);
    register_noop_command(engine, "@gobbletwo", 2);
    register_noop_command(engine, "centering", 0);
    register_noop_command(engine, "date", 1);
    register_noop_command(engine, "footnote", 1);
    register_noop_command(engine, "hfill", 0);
    register_noop_command(engine, "title", 1);
    register_noop_command(engine, "textbf", 1);
    register_noop_command(engine, "textit", 1);
    register_noop_command(engine, "emph", 1);
    register_noop_command(engine, "textsc", 1);
    register_noop_command(engine, "textsf", 1);
    register_noop_command(engine, "texttt", 1);
    register_noop_command(engine, "underline", 1);
    register_noop_command(engine, "today", 0);
    register_noop_command(engine, "vspace", 1);
    register_noop_command(engine, "hspace", 1);
    register_noop_command(engine, "bigskip", 0);
    register_noop_command(engine, "medskip", 0);
    register_noop_command(engine, "smallskip", 0);
    register_noop_command(engine, "setlength", 2);
    register_noop_command(engine, "addtolength", 2);
    register_noop_command(engine, "quad", 0);
    register_noop_command(engine, "qquad", 0);
    register_noop_command(engine, "noindent", 0);
    register_passthrough_command(engine, "@firstoftwo", 2, 1);
    register_passthrough_command(engine, "@secondoftwo", 2, 2);
}

pub fn load_package(
    name: &str,
    options: &[String],
    registry: &mut PackageRegistry,
    engine: &mut MacroEngine,
    active_class: Option<ClassInfo>,
    sty_resolver: Option<&StyPackageResolver<'_>>,
) -> Result<bool, String> {
    if registry.is_loaded(name) {
        return Ok(false);
    }

    let registered = registry.load(PackageInfo {
        name: name.to_string(),
        options: options.to_vec(),
    });
    debug_assert!(registered);

    let result = if let Some(extension) = get_native_extension(name) {
        extension.register(engine);
        Ok(())
    } else if let Some(resolve_sty) = sty_resolver {
        if let Some(source) = resolve_sty(name) {
            StyInterpreter::new(
                &source,
                options,
                registry,
                engine,
                active_class,
                sty_resolver,
            )
            .interpret()
        } else {
            Ok(())
        }
    } else {
        Ok(())
    };

    if let Err(error) = result {
        registry.unload(name);
        return Err(error);
    }

    Ok(true)
}

fn get_native_extension(name: &str) -> Option<&'static dyn PackageExtension> {
    static AMSMATH_EXTENSION: AmsmathExtension = AmsmathExtension;
    static FONTSPEC_EXTENSION: FontspecExtension = FontspecExtension;
    static GEOMETRY_EXTENSION: GeometryExtension = GeometryExtension;
    static GRAPHICX_EXTENSION: GraphicxExtension = GraphicxExtension;
    static XCOLOR_EXTENSION: XcolorExtension = XcolorExtension;
    static MULTICOL_EXTENSION: MulticolExtension = MulticolExtension;

    match name {
        "amsmath" => Some(&AMSMATH_EXTENSION),
        "fontspec" => Some(&FONTSPEC_EXTENSION),
        "geometry" => Some(&GEOMETRY_EXTENSION),
        "graphicx" => Some(&GRAPHICX_EXTENSION),
        "xcolor" => Some(&XCOLOR_EXTENSION),
        "multicol" => Some(&MULTICOL_EXTENSION),
        _ => None,
    }
}

/// Packages that ferritex handles directly in the parser/runtime without a
/// `PackageExtension` entry (for example via dedicated command handlers).
///
/// `tikz`/`pgf` — `\begin{tikzpicture}` is parsed by the kernel directly
/// (see graphics/tikz.rs), so `\usepackage{tikz}` should not warn.
/// `hyperref` — `\href`, `\hyperref`, `\url`, `\hypersetup` have dedicated
/// parser handlers.
/// `url`/`color` — `\url`, `\color`, `\textcolor`, `\definecolor` have
/// kernel-level handlers (xcolor extension is a separate, equivalent path).
fn is_kernel_integrated_package(name: &str) -> bool {
    matches!(name, "hyperref" | "tikz" | "pgf" | "url" | "color")
}

/// Returns whether `\usepackage{name}` would resolve to a ferritex-supported
/// implementation. A package is considered "implemented" if it has a native
/// extension, is integrated into the parser kernel, or can be interpreted
/// from a `.sty` source via the supplied resolver.
///
/// When this returns `false`, loading the package still succeeds, but its
/// commands will typically surface as undefined control sequences at use
/// site. Callers emit a non-fatal warning so users see the gap at preamble
/// time rather than deep in the document body.
pub fn is_implemented_package(
    name: &str,
    sty_resolver: Option<&StyPackageResolver<'_>>,
) -> bool {
    if get_native_extension(name).is_some() {
        return true;
    }
    if is_kernel_integrated_package(name) {
        return true;
    }
    sty_resolver.is_some_and(|resolver| resolver(name).is_some())
}

fn register_transparent_environment(engine: &mut MacroEngine, name: &str) {
    engine.define_global_environment(
        name.to_string(),
        EnvironmentDef {
            name: name.to_string(),
            begin_tokens: Vec::new(),
            end_tokens: Vec::new(),
            parameter_count: 0,
        },
    );
}

fn register_minipage_environment(engine: &mut MacroEngine) {
    engine.define_global_environment(
        "minipage".to_string(),
        EnvironmentDef {
            name: "minipage".to_string(),
            begin_tokens: Vec::new(),
            end_tokens: Vec::new(),
            parameter_count: 1,
        },
    );
}

fn register_noop_command(engine: &mut MacroEngine, name: &str, parameter_count: usize) {
    engine.define_global(
        name.to_string(),
        MacroDef {
            name: name.to_string(),
            parameter_count,
            body: Vec::new(),
            protected: false,
        },
    );
}

fn register_alias_command(engine: &mut MacroEngine, name: &str, target: &str) {
    engine.define_global(
        name.to_string(),
        MacroDef {
            name: name.to_string(),
            parameter_count: 0,
            body: vec![Token {
                kind: TokenKind::ControlWord(target.to_string()),
                line: 0,
                column: 0,
            }],
            protected: false,
        },
    );
}

fn register_passthrough_command(
    engine: &mut MacroEngine,
    name: &str,
    parameter_count: usize,
    passthrough_parameter: u8,
) {
    engine.define_global(
        name.to_string(),
        MacroDef {
            name: name.to_string(),
            parameter_count,
            body: vec![Token {
                kind: TokenKind::Parameter(passthrough_parameter),
                line: 0,
                column: 0,
            }],
            protected: false,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::{
        load_document_class, load_package, register_base_latex_commands, ClassInfo, ClassRegistry,
        OptionRegistry, PackageInfo, PackageRegistry,
    };
    use crate::parser::{CatCode, MacroDef, MacroEngine, Token, TokenKind};

    const FTXUTILS_STY: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../ferritex-bench/fixtures/bundle/texmf/tex/latex/ftxutils.sty"
    ));
    const FTXCOMPAT_STY: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../ferritex-bench/fixtures/bundle/texmf/tex/latex/ftxcompat.sty"
    ));
    const FTXDEPCHAIN_STY: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../ferritex-bench/fixtures/bundle/texmf/tex/latex/ftxdepchain.sty"
    ));

    fn ftx_bundle_source(name: &str) -> Option<&'static str> {
        match name {
            "ftxutils" => Some(FTXUTILS_STY),
            "ftxcompat" => Some(FTXCOMPAT_STY),
            "ftxdepchain" => Some(FTXDEPCHAIN_STY),
            _ => None,
        }
    }

    fn expanded_macro_text(engine: &MacroEngine, name: &str) -> Option<String> {
        engine
            .lookup(name)
            .map(|definition| expand_token_text(engine, &definition.body, 0))
    }

    fn expand_token_text(engine: &MacroEngine, tokens: &[Token], depth: usize) -> String {
        assert!(depth < 16, "macro expansion depth exceeded");

        let mut text = String::new();
        for token in tokens {
            match &token.kind {
                TokenKind::CharToken { char, .. } => text.push(*char),
                TokenKind::ControlWord(name) => {
                    if let Some(definition) = engine.lookup(name) {
                        if definition.parameter_count == 0 {
                            text.push_str(&expand_token_text(
                                engine,
                                &engine.expand(name, &[]),
                                depth + 1,
                            ));
                        }
                    }
                }
                _ => {}
            }
        }

        text
    }

    #[test]
    fn package_registry_prevents_duplicate_loads() {
        let mut registry = PackageRegistry::default();

        assert!(registry.load(PackageInfo {
            name: "amsmath".to_string(),
            options: vec!["fleqn".to_string()],
        }));
        assert!(!registry.load(PackageInfo {
            name: "amsmath".to_string(),
            options: vec!["reqno".to_string()],
        }));
        assert_eq!(
            registry.loaded_packages(),
            &[PackageInfo {
                name: "amsmath".to_string(),
                options: vec!["fleqn".to_string()],
            }]
        );
    }

    #[test]
    fn class_registry_tracks_active_class() {
        let mut registry = ClassRegistry::default();
        registry.set_class(ClassInfo {
            name: "report".to_string(),
            options: vec!["11pt".to_string()],
        });

        assert_eq!(
            registry.active_class(),
            Some(&ClassInfo {
                name: "report".to_string(),
                options: vec!["11pt".to_string()],
            })
        );
    }

    #[test]
    fn load_document_class_registers_class_specific_commands() {
        let mut registry = ClassRegistry::default();
        let mut engine = MacroEngine::default();

        load_document_class("report", &["11pt".to_string()], &mut registry, &mut engine)
            .expect("load report class");

        assert_eq!(
            registry.active_class(),
            Some(&ClassInfo {
                name: "report".to_string(),
                options: vec!["11pt".to_string()],
            })
        );
        assert!(engine.lookup("chapter").is_some());
        assert!(engine.lookup("section").is_some());
        assert!(engine.lookup_environment("itemize").is_some());
    }

    #[test]
    fn load_letter_class_registers_letter_specific_commands() {
        let mut registry = ClassRegistry::default();
        let mut engine = MacroEngine::default();

        load_document_class("letter", &[], &mut registry, &mut engine).expect("load letter class");

        let opening = engine.lookup("opening").expect("\\opening defined");
        assert_eq!(opening.parameter_count, 1);
        let closing = engine.lookup("closing").expect("\\closing defined");
        assert_eq!(closing.parameter_count, 1);
    }

    #[test]
    fn base_latex_registration_defines_kernel_footnote_command() {
        let mut engine = MacroEngine::default();

        register_base_latex_commands(&mut engine);

        let footnote = engine.lookup("footnote").expect("\\footnote defined");
        assert_eq!(footnote.parameter_count, 1);
    }

    #[test]
    fn base_latex_registration_consumes_minipage_args_and_ignores_layout_hints() {
        let mut engine = MacroEngine::default();

        register_base_latex_commands(&mut engine);

        let minipage = engine
            .lookup_environment("minipage")
            .expect("minipage environment");
        assert_eq!(minipage.parameter_count, 1);
        assert!(minipage.begin_tokens.is_empty());

        assert_eq!(
            engine
                .lookup("centering")
                .map(|definition| definition.parameter_count),
            Some(0)
        );
        assert_eq!(
            engine
                .lookup("hfill")
                .map(|definition| definition.parameter_count),
            Some(0)
        );
    }

    #[test]
    fn load_package_registers_native_extensions_once() {
        let mut registry = PackageRegistry::default();
        let mut engine = MacroEngine::default();

        assert!(
            load_package("xcolor", &[], &mut registry, &mut engine, None, None)
                .expect("load xcolor")
        );
        assert!(!load_package(
            "xcolor",
            &["dvipsnames".to_string()],
            &mut registry,
            &mut engine,
            None,
            None,
        )
        .expect("duplicate xcolor load"));

        assert!(engine.lookup("color").is_some());
        assert!(engine.lookup("textcolor").is_some());
        assert_eq!(registry.loaded_packages().len(), 1);
    }

    #[test]
    fn fontspec_extension_registers_all_font_commands() {
        let mut registry = PackageRegistry::default();
        let mut engine = MacroEngine::default();

        assert!(
            load_package("fontspec", &[], &mut registry, &mut engine, None, None)
                .expect("load fontspec")
        );

        assert!(engine.lookup("setmainfont").is_some());
        assert!(engine.lookup("setsansfont").is_some());
        assert!(engine.lookup("setmonofont").is_some());
    }

    #[test]
    fn load_package_interprets_sty_fallback_and_recurses_requirepackage() {
        let mut registry = PackageRegistry::default();
        let mut engine = MacroEngine::default();
        let resolver = |name: &str| match name {
            "mypkg" => Some(
                "\\NeedsTeXFormat{LaTeX2e}\n\
                 \\ProvidesPackage{mypkg}[2024/01/01 Test package]\n\
                 \\newif\\ifmypkgbold\n\
                 \\RequirePackage{amsmath}\n\
                 \\DeclareOption{bold}{\\mypkgboldtrue}\n\
                 \\DeclareOption*{}\n\
                 \\ProcessOptions*\n\
                 \\@ifpackageloaded{amsmath}{\\def\\amsloaded{yes}}{\\def\\amsloaded{no}}\n\
                 \\makeatletter\n\
                 \\@namedef{mypkg@flag}{set}\n\
                 \\@ifundefined{mypkg@missing}{\\def\\mypkgmissing{yes}}{\\def\\mypkgmissing{no}}\n\
                 \\makeatother\n\
                 \\ifmypkgbold\\def\\mypkgstyle{bold}\\else\\def\\mypkgstyle{plain}\\fi\n\
                 \\newcommand{\\mypkgcmd}[1]{[#1]}\n\
                 \\newenvironment{mypkgenv}{\\begin{center}}{\\end{center}}\n\
                 \\input{ignored}\n"
                    .to_string(),
            ),
            _ => None,
        };

        assert!(load_package(
            "mypkg",
            &["bold".to_string()],
            &mut registry,
            &mut engine,
            None,
            Some(&resolver),
        )
        .expect("load mypkg"));
        assert!(!load_package(
            "mypkg",
            &[],
            &mut registry,
            &mut engine,
            None,
            Some(&resolver)
        )
        .expect("duplicate mypkg load"));

        assert!(registry.is_loaded("mypkg"));
        assert!(registry.is_loaded("amsmath"));
        assert!(engine.lookup("mypkgcmd").is_some());
        assert!(engine.lookup_environment("mypkgenv").is_some());
        assert_eq!(
            engine
                .lookup("mypkgstyle")
                .map(|definition| token_text(&definition.body)),
            Some("bold".to_string())
        );
        assert_eq!(
            engine
                .lookup("amsloaded")
                .map(|definition| token_text(&definition.body)),
            Some("yes".to_string())
        );
        assert_eq!(
            engine
                .lookup("mypkgmissing")
                .map(|definition| token_text(&definition.body)),
            Some("yes".to_string())
        );
        assert!(engine.lookup("ifmypkgbold").is_some());
        assert!(engine.lookup("mypkgboldtrue").is_some());
        assert!(engine.lookup("mypkgboldfalse").is_some());
        assert!(engine.lookup("mypkg@flag").is_some());
    }

    #[test]
    fn load_package_interprets_ftxutils_sty_directly() {
        let mut registry = PackageRegistry::default();
        let mut engine = MacroEngine::default();
        let resolver = |name: &str| ftx_bundle_source(name).map(ToOwned::to_owned);

        assert!(load_package(
            "ftxutils",
            &[],
            &mut registry,
            &mut engine,
            None,
            Some(&resolver)
        )
        .expect("load ftxutils"));

        assert!(registry.is_loaded("ftxutils"));
        assert_eq!(
            expanded_macro_text(&engine, "ftxutilsinfo"),
            Some("UTILS:utils-defined-ok".to_string())
        );
        assert!(engine.lookup("ftxutils@version").is_some());
        assert_eq!(
            engine
                .lookup("ftxutilscheck")
                .map(|definition| token_text(&definition.body)),
            Some("utils-defined-ok".to_string())
        );
    }

    #[test]
    fn load_package_interprets_ftxcompat_sty_with_options() {
        let mut registry = PackageRegistry::default();
        let mut engine = MacroEngine::default();
        let resolver = |name: &str| ftx_bundle_source(name).map(ToOwned::to_owned);

        assert!(load_package(
            "ftxcompat",
            &["draft".to_string()],
            &mut registry,
            &mut engine,
            None,
            Some(&resolver),
        )
        .expect("load ftxcompat"));

        assert!(registry.is_loaded("ftxcompat"));
        assert!(registry.is_loaded("ftxutils"));
        assert_eq!(
            engine
                .lookup("ftxcompatstyle")
                .map(|definition| token_text(&definition.body)),
            Some("draft-mode".to_string())
        );
        assert_eq!(
            engine
                .lookup("ftxcompatdep")
                .map(|definition| token_text(&definition.body)),
            Some("compat-loaded-ftxutils".to_string())
        );
        assert!(engine.lookup("ifftxcompatdraft").is_some());
        assert!(engine.lookup("ftxcompatdrafttrue").is_some());
        assert!(engine.lookup("ftxcompatdraftfalse").is_some());
    }

    #[test]
    fn load_package_interprets_ftxdepchain_recursive_dependencies() {
        let mut registry = PackageRegistry::default();
        let mut engine = MacroEngine::default();
        let resolver = |name: &str| ftx_bundle_source(name).map(ToOwned::to_owned);

        assert!(load_package(
            "ftxdepchain",
            &[],
            &mut registry,
            &mut engine,
            None,
            Some(&resolver),
        )
        .expect("load ftxdepchain"));

        assert!(registry.is_loaded("ftxdepchain"));
        assert!(registry.is_loaded("ftxcompat"));
        assert!(registry.is_loaded("ftxutils"));
        assert_eq!(
            engine
                .lookup("ftxdepchainroot")
                .map(|definition| token_text(&definition.body)),
            Some("chain-loaded-compat".to_string())
        );
        assert_eq!(
            engine
                .lookup("ftxdepchaintrans")
                .map(|definition| token_text(&definition.body)),
            Some("chain-has-utils".to_string())
        );
        assert_eq!(
            engine
                .lookup("ftxcompatstyle")
                .map(|definition| token_text(&definition.body)),
            Some("final-mode".to_string())
        );
    }

    #[test]
    fn load_package_rolls_back_failed_sty_without_losing_existing_state() {
        let mut registry = PackageRegistry::default();
        let mut engine = MacroEngine::default();
        let resolver = |name: &str| match name {
            "brokenpkg" => Some("\\def\\broken{X".to_string()),
            _ => None,
        };

        assert!(
            load_package("xcolor", &[], &mut registry, &mut engine, None, None)
                .expect("load xcolor")
        );
        engine.define_global(
            "survivor".to_string(),
            MacroDef {
                name: "survivor".to_string(),
                parameter_count: 0,
                body: token_code("S"),
                protected: false,
            },
        );

        let error = load_package(
            "brokenpkg",
            &[],
            &mut registry,
            &mut engine,
            None,
            Some(&resolver),
        )
        .expect_err("broken .sty should fail");

        assert!(error.contains("brace") || error.contains("Brace"));
        assert!(registry.is_loaded("xcolor"));
        assert!(!registry.is_loaded("brokenpkg"));
        assert!(engine.lookup("color").is_some());
        assert!(engine.lookup("survivor").is_some());
        assert!(engine.lookup("broken").is_none());
    }

    #[test]
    fn option_registry_processes_options_in_declaration_order() {
        let mut registry = OptionRegistry::default();
        registry.declare_option("beta".to_string(), token_code("B"));
        registry.declare_option("alpha".to_string(), token_code("A"));

        assert_eq!(
            token_text(&registry.process_options(&["alpha".to_string(), "beta".to_string()])),
            "BA"
        );
    }

    #[test]
    fn option_registry_uses_default_handler_for_unknown_execute_option() {
        let mut registry = OptionRegistry::default();
        registry.declare_default(token_code("D"));

        assert_eq!(
            token_text(&registry.execute_options(&["unknown".to_string()])),
            "D"
        );
    }

    fn token_code(text: &str) -> Vec<Token> {
        text.chars()
            .enumerate()
            .map(|(index, char)| Token {
                kind: TokenKind::CharToken {
                    char,
                    cat: if char.is_ascii_alphabetic() {
                        CatCode::Letter
                    } else {
                        CatCode::Other
                    },
                },
                line: 1,
                column: (index + 1) as u32,
            })
            .collect()
    }

    fn token_text(tokens: &[Token]) -> String {
        tokens
            .iter()
            .filter_map(|token| match token.kind {
                TokenKind::CharToken { char, .. } => Some(char),
                _ => None,
            })
            .collect()
    }
}
