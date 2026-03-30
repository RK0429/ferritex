pub mod harness;
pub mod parity;

pub use harness::{
    bench_fixtures_root, bundle_bootstrap_cases, bundle_package_loading_cases,
    corpus_bibliography_cases, corpus_compat_cases, corpus_embedded_assets_cases,
    corpus_navigation_cases, corpus_partition_article_cases, corpus_partition_book_cases,
    corpus_tikz_basic_shapes_cases, corpus_tikz_nested_cases, full_bench_cases,
    partition_bench_cases, BenchCase, BenchComparison, BenchFailure, BenchHarness, BenchProfile,
    BenchReport, BenchResult, BenchRunConfig, BenchTiming, CliCompileBackend, CompileBackend,
    CompileOutput,
};
pub use parity::{
    compute_parity_score, extract_line_y_positions, extract_pdf_page_count, format_parity_summary,
    ParityResult, ParityScore,
};
