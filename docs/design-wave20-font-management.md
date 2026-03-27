# Design: Wave 20 — Named Font Resolution & Minimal fontspec Foundation

## Meta

| Item | Value |
|---|---|
| Date | 2026-03-27 |
| Status | Draft — for orchestrator review |
| Scope | REQ-FUNC-019 (font resolution, v1 subset), REQ-FUNC-025 (fontspec, v1 subset) |
| Related | REQ-FUNC-017 (OpenType loading), REQ-FUNC-018 (TFM loading) |

---

## 1. Current State & Problem

### What exists

| Area | Status | Location |
|---|---|---|
| OpenType parser | Full: `cmap`, `head`, `hhea`, `hmtx`, subsetting, glyph metrics | `core/font/opentype.rs` |
| TFM parser | Full: char widths, ligature/kerning | `core/font/tfm.rs` |
| OpenType → PDF embedding | Working: TrueType subset, ToUnicode, width array | `application/compile_job_service.rs:1475-1542` |
| Font loading in compile | **First-parseable-TTF**: walks `OPENTYPE_FONT_SEARCH_ROOTS` in asset bundle, returns first TTF that parses | `application/compile_job_service.rs:1367-1428` |
| `\usepackage` handling | Package name registered in `PackageRegistry`, native extensions dispatched | `core/parser/package_loading.rs` |
| `PackageExtension` trait | Exists; impls for `amsmath`, `graphicx`, `xcolor`, `geometry` | `core/parser/package_loading.rs` |

### What is missing (Wave 20 scope)

1. **Named font resolution**: No way to resolve a font by family name (e.g. `"Noto Serif"`, `"TexGyreTermes"`). Current code picks the first parseable TTF file found in directory walk.
2. **`\usepackage{fontspec}` recognition**: Parser has no `FontspecExtension`. The package name `fontspec` is registered but triggers no behavior.
3. **`\setmainfont{...}` parsing**: No preamble command handling for font selection. The compile service has no input for "which font to use by name".
4. **Project-local font lookup**: Fonts in the document's directory/project are not searched. Only the asset bundle is searched.
5. **Font name → file mapping**: No index that maps a font family name to a TTF/OTF file path within the asset bundle or project tree.

### What is explicitly OUT OF SCOPE for Wave 20

- `OverlaySet` entity and configured overlay roots (requires `REQ-FUNC-026` generic `.sty` loading infra)
- Host Font Catalog / platform font discovery (`fontconfig`, `CoreText`, `DirectWrite`)
- `\setsansfont`, `\setmonofont` (sans/mono family selection)
- Mid-document font switching (`\fontspec`, local `\setmainfont` in body)
- OpenType features (`Ligatures`, `Numbers`, `Script`, `Language` options)
- `FontFallbackChain`, `FontFeatureSet`, `FontResolverCache` (domain model entities deferred)
- `FontWeight`, `FontStyle` matching (bold/italic variant selection)

---

## 2. Design Decisions

### D1: Two-tier font search — project-local then asset-bundle

**Resolution order** (simplified v1 of `OverlaySet`):

1. **Project-local**: `{input_dir}/` and `{project_root}/fonts/` — flat scan for `.ttf`/`.otf` files
2. **Asset bundle**: existing `OPENTYPE_FONT_SEARCH_ROOTS` — recursive walk (same as today)

**Name matching**: case-insensitive comparison of the requested family name against:
- File stem (e.g. `NotoSerif-Regular.ttf` → `"NotoSerif-Regular"`, `"notoserif-regular"`)
- OpenType `name` table entry ID 1 (Font Family) and ID 4 (Full Font Name), if parseable

**Rationale**: This is the minimal useful behavior. XeTeX and LuaTeX both search project-local first, then system/texmf. We skip the `name` table parsing in v1 if it proves too costly — file stem matching alone is sufficient for asset-bundle fonts where file names are conventionally meaningful.

**ADR**: D1 establishes a font resolution function with a defined priority order. When `OverlaySet` is implemented (future wave), this function becomes a method on `OverlaySet` and the resolution order expands to include configured overlay roots and host-font catalog.

### D2: fontspec extension registers `\setmainfont` as a preamble-only command

The `FontspecExtension` implements `PackageExtension` and registers a single command `\setmainfont` that:
- Reads one required braced argument (the font family name)
- Stores it on a new field `main_font_name: Option<String>` on `ParsedDocument`
- Is only effective in the preamble (before `\begin{document}`). If encountered in the body, emit a warning diagnostic and ignore.

**No other fontspec commands** are registered in v1.

**Rationale**: `\setmainfont` in the preamble with a document-global effect is the overwhelmingly common usage pattern. This avoids the complexity of mid-document font switching, font stacks, and the full fontspec option parser.

### D3: Font name flows from parser → compile service → font resolver

Data flow:

```
Source ──parser──► ParsedDocument.main_font_name: Option<String>
                         │
CompileJobService reads ◄┘
                         │
         ┌───────────────┘
         ▼
resolve_named_font(name, input_dir, project_root, asset_bundle_path)
         │
         ▼
    Option<LoadedOpenTypeFont>
         │
    (existing OpenType → PDF path unchanged)
```

If `main_font_name` is `Some(name)`:
- Call `resolve_named_font(name, ...)` → returns `Option<LoadedOpenTypeFont>` on match
- If `None` (not found), emit a diagnostic error `"Font not found: {name}"` and fall back to the existing first-parseable-TTF behavior

If `main_font_name` is `None` (no `\setmainfont`, or no fontspec):
- Existing behavior unchanged (first parseable TTF from asset bundle)

**Rationale**: Minimal disruption. The only new parser output is one `Option<String>`. The existing OpenType→typesetting→PDF pipeline is reused entirely.

### D4: Diagnostics

| Condition | Severity | Message |
|---|---|---|
| `\setmainfont{X}` and font `X` found | — | (silent success) |
| `\setmainfont{X}` and font `X` not found | Error | `Font "{X}" not found in project directory or asset bundle` |
| `\setmainfont{X}` used in document body | Warning | `\setmainfont in document body is not supported; use it in the preamble` |
| `\usepackage{fontspec}` without `\setmainfont` | — | (silent; existing first-TTF behavior applies) |
| `\setmainfont{X}` without `\usepackage{fontspec}` | Warning | `\setmainfont requires \usepackage{fontspec}` |

---

## 3. File Changes

### ferritex-core

| File | Change | Lines (est.) |
|---|---|---|
| `parser/package_loading.rs` | Add `FontspecExtension` implementing `PackageExtension`; register it in `get_native_extension()` for `"fontspec"` | ~30 |
| `parser/api.rs` | Add `main_font_name: Option<String>` to `ParsedDocument`; handle `"setmainfont"` in preamble command dispatch (read braced arg, store name) | ~25 |
| `parser/api.rs` | If `\setmainfont` encountered after `\begin{document}`, push warning `ParseError` | ~10 |
| `font/mod.rs` | Add `pub mod resolver;` | ~1 |
| `font/resolver.rs` | **New file**: `resolve_named_font(name, input_dir, project_root, asset_bundle_path, file_access_gate) -> Option<(PathBuf, OpenTypeFont)>`. Implements D1 search order with file-stem matching. | ~80 |

### ferritex-application

| File | Change | Lines (est.) |
|---|---|---|
| `compile_job_service.rs` | In `compile()`: after parse, read `document.main_font_name`; if `Some`, call `resolve_named_font()` instead of `load_opentype_font()`; emit diagnostic if not found; fall back to existing behavior | ~30 |
| `compile_job_service.rs` | Add diagnostic generation for font-not-found and body-setmainfont cases | ~15 |

### No changes to

- `ferritex-infra` (asset bundle loader interface unchanged)
- `ferritex-core/font/opentype.rs` (OpenType parser unchanged)
- `ferritex-core/font/tfm.rs` (TFM parser unchanged)
- `ferritex-core/pdf/` (PDF renderer unchanged — receives same `FontResource`)
- `ports.rs` (no new ports needed; `FileAccessGate` already provides file read)

---

## 4. Key Interface

```rust
// ferritex-core/src/font/resolver.rs

/// Result of named font resolution.
pub struct ResolvedFont {
    pub path: PathBuf,
    pub font: OpenTypeFont,
    pub base_font_name: String,
}

/// Resolve a font by family name.
///
/// Search order:
/// 1. Project-local: `input_dir/` flat, then `project_root/fonts/` flat
/// 2. Asset bundle: recursive walk of OPENTYPE_FONT_SEARCH_ROOTS
///
/// Matching: case-insensitive comparison against file stem.
/// Returns `None` if no matching font is found.
pub fn resolve_named_font(
    name: &str,
    input_dir: &Path,
    project_root: &Path,
    asset_bundle_path: Option<&Path>,
    file_access_gate: &dyn FileAccessGate,
) -> Option<ResolvedFont> { ... }
```

```rust
// Addition to ParsedDocument (parser/api.rs)

pub struct ParsedDocument {
    // ... existing fields ...
    /// Font family name from preamble `\setmainfont{...}`, if fontspec is loaded.
    pub main_font_name: Option<String>,
}
```

---

## 5. Test Plan

### Unit tests — `ferritex-core`

| # | Test | Location | Description |
|---|---|---|---|
| T1 | `fontspec_extension_registers_setmainfont` | `parser/package_loading.rs` | `FontspecExtension::register()` adds `"setmainfont"` to engine |
| T2 | `setmainfont_in_preamble_sets_main_font_name` | `parser/api.rs` | Parse `\documentclass{article}\usepackage{fontspec}\setmainfont{TestFont}\begin{document}Body\end{document}` → `main_font_name == Some("TestFont")` |
| T3 | `setmainfont_without_fontspec_emits_warning` | `parser/api.rs` | Parse `\documentclass{article}\setmainfont{TestFont}\begin{document}Body\end{document}` → warning in errors, `main_font_name == None` |
| T4 | `setmainfont_in_body_emits_warning` | `parser/api.rs` | Parse with `\setmainfont` after `\begin{document}` → warning, `main_font_name` retains preamble value or `None` |
| T5 | `no_fontspec_produces_none_main_font_name` | `parser/api.rs` | Standard document without fontspec → `main_font_name == None` |
| T6 | `resolve_named_font_finds_project_local_font` | `font/resolver.rs` | Create temp dir with `TestFont.ttf`, call resolver with `"TestFont"` → returns the font |
| T7 | `resolve_named_font_finds_asset_bundle_font` | `font/resolver.rs` | Create temp bundle with `texmf/fonts/truetype/TestFont.ttf`, call resolver → returns the font |
| T8 | `resolve_named_font_project_local_takes_priority` | `font/resolver.rs` | Same font name in both locations → project-local path returned |
| T9 | `resolve_named_font_case_insensitive` | `font/resolver.rs` | Request `"testfont"` when file is `TestFont.ttf` → found |
| T10 | `resolve_named_font_returns_none_when_not_found` | `font/resolver.rs` | Request `"Nonexistent"` → `None` |

### Integration tests — `ferritex-application`

| # | Test | Description |
|---|---|---|
| T11 | `compile_with_setmainfont_uses_named_font` | Full compile with fontspec + `\setmainfont{TestSans}` where `TestSans.ttf` is in the asset bundle → PDF embeds that font, not a random first TTF |
| T12 | `compile_with_setmainfont_not_found_emits_diagnostic` | `\setmainfont{Missing}` → compile succeeds with fallback font + error diagnostic in output |
| T13 | `compile_without_fontspec_uses_first_ttf_behavior` | Existing behavior preserved — no fontspec → first parseable TTF from bundle |

---

## 6. Migration & Compatibility

- **No breaking changes**: Existing documents without `\usepackage{fontspec}` behave identically to today.
- **New behavior is additive**: Only documents that explicitly load fontspec and use `\setmainfont` get named resolution.
- **Forward path to full OverlaySet**: `resolve_named_font` is a standalone function, not a method on a global service. When `OverlaySet` is implemented, the function body becomes `OverlaySet::resolve_font()` and the standalone function is removed.

---

## 7. Non-Goals Reiteration

The following are **not** addressed by this design and should be tracked as follow-up waves:

1. **Configured overlay roots** — requires `OverlaySet` entity and CLI `--overlay` flag
2. **Host Font Catalog fallback** — requires platform font discovery integration
3. **`\setsansfont` / `\setmonofont`** — requires font-family-to-role mapping in typesetting
4. **Mid-document font switching** — requires per-paragraph font state in typesetter
5. **OpenType feature options** — requires `FontFeatureSet` parsing and GSUB/GPOS application
6. **Bold/italic variant matching** — requires `FontWeight`/`FontStyle` + variant file discovery
7. **`name` table parsing for font family matching** — can be added incrementally to `resolve_named_font` without API changes
