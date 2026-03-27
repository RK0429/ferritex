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

/// Trait for native package extensions that register commands/environments
pub trait PackageExtension {
    fn name(&self) -> &str;
    fn register(&self, engine: &mut MacroEngine);
}

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

impl PackageExtension for AmsmathExtension {
    fn name(&self) -> &str {
        "amsmath"
    }

    fn register(&self, engine: &mut MacroEngine) {
        for name in [
            "align",
            "align*",
            "equation*",
            "gather",
            "gather*",
            "multline",
            "multline*",
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
        "report" | "book" => {
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
        "letter" => {
            class_registry.set_class(ClassInfo {
                name: name.to_string(),
                options: options.to_vec(),
            });
            register_base_latex_commands(engine);
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
        "minipage",
        "quote",
        "quotation",
        "table",
        "tabular",
        "thebibliography",
        "verse",
    ] {
        register_transparent_environment(engine, name);
    }

    register_noop_command(engine, "author", 1);
    register_noop_command(engine, "maketitle", 0);
    register_noop_command(engine, "title", 1);
}

pub fn load_package(
    name: &str,
    options: &[String],
    registry: &mut PackageRegistry,
    engine: &mut MacroEngine,
) -> Result<bool, String> {
    if registry.is_loaded(name) {
        return Ok(false);
    }

    if let Some(extension) = get_native_extension(name) {
        extension.register(engine);
    }

    Ok(registry.load(PackageInfo {
        name: name.to_string(),
        options: options.to_vec(),
    }))
}

fn get_native_extension(name: &str) -> Option<&'static dyn PackageExtension> {
    static AMSMATH_EXTENSION: AmsmathExtension = AmsmathExtension;
    static FONTSPEC_EXTENSION: FontspecExtension = FontspecExtension;
    static GEOMETRY_EXTENSION: GeometryExtension = GeometryExtension;
    static GRAPHICX_EXTENSION: GraphicxExtension = GraphicxExtension;
    static XCOLOR_EXTENSION: XcolorExtension = XcolorExtension;

    match name {
        "amsmath" => Some(&AMSMATH_EXTENSION),
        "fontspec" => Some(&FONTSPEC_EXTENSION),
        "geometry" => Some(&GEOMETRY_EXTENSION),
        "graphicx" => Some(&GRAPHICX_EXTENSION),
        "xcolor" => Some(&XCOLOR_EXTENSION),
        _ => None,
    }
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

fn register_noop_command(engine: &mut MacroEngine, name: &str, parameter_count: usize) {
    engine.define_global(
        name.to_string(),
        MacroDef {
            name: name.to_string(),
            parameter_count,
            body: Vec::new(),
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
        },
    );
}

#[cfg(test)]
mod tests {
    use super::{
        load_document_class, load_package, ClassInfo, ClassRegistry, PackageInfo, PackageRegistry,
    };
    use crate::parser::MacroEngine;

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
    fn load_package_registers_native_extensions_once() {
        let mut registry = PackageRegistry::default();
        let mut engine = MacroEngine::default();

        assert!(load_package("xcolor", &[], &mut registry, &mut engine).expect("load xcolor"));
        assert!(!load_package(
            "xcolor",
            &["dvipsnames".to_string()],
            &mut registry,
            &mut engine
        )
        .expect("duplicate xcolor load"));

        assert!(engine.lookup("color").is_some());
        assert!(engine.lookup("textcolor").is_some());
        assert_eq!(registry.loaded_packages().len(), 1);
    }

    #[test]
    fn fontspec_extension_registers_setmainfont() {
        let mut registry = PackageRegistry::default();
        let mut engine = MacroEngine::default();

        assert!(load_package("fontspec", &[], &mut registry, &mut engine).expect("load fontspec"));

        assert!(engine.lookup("setmainfont").is_some());
    }
}
