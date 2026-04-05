pub mod harness;
pub mod parity;

pub use harness::{
    bench_fixtures_root, bundle_bootstrap_cases, bundle_package_loading_cases,
    bundle_reproducible_cases, corpus_bibliography_cases, corpus_combined_stress_cases,
    corpus_compat_cases, corpus_embedded_assets_cases, corpus_navigation_cases,
    corpus_partition_article_cases, corpus_partition_book_cases,
    corpus_partition_book_parity_cases, corpus_tikz_basic_shapes_cases, corpus_tikz_nested_cases,
    full_bench_cases, full_bench_strict_cases, partition_bench_cases, stress_bench_cases,
    BenchCase, BenchComparison, BenchFailure, BenchHarness, BenchProfile, BenchReport, BenchResult,
    BenchRunConfig, BenchTiming, CliCompileBackend, CompileBackend, CompileOutput,
};
pub use parity::{
    compute_bibliography_parity_score, compute_embedded_assets_parity_score,
    compute_navigation_parity_score, compute_parity_score, compute_tikz_parity_score,
    extract_bibliography_manifest, extract_embedded_assets_manifest, extract_graphics_ops,
    extract_line_y_positions, extract_navigation_manifest, extract_pdf_page_count,
    format_bibliography_parity_summary, format_embedded_assets_parity_summary,
    format_navigation_parity_summary, format_parity_summary, format_tikz_parity_summary,
    BibliographyManifest, BibliographyParityResult, BibliographyParityScore,
    EmbeddedAssetsManifest, EmbeddedAssetsParityResult, EmbeddedAssetsParityScore, GraphicsOp,
    NavigationManifest, NavigationParityResult, NavigationParityScore, ParityResult, ParityScore,
    TikzParityResult, TikzParityScore,
};
