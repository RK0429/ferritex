# partition-article reference PDFs

- Generated with: `/Library/TeX/texbin/pdflatex`
- pdfLaTeX version:
  - `pdfTeX 3.141592653-2.6-1.40.28 (TeX Live 2025)`
  - `kpathsea version 6.4.1`
- Date generated: `2026-04-02 10:34:38 JST`
- Working directory: `ferritex/crates/ferritex-bench/fixtures/corpus/partition-article/reference/`
- Exact generation command used:
  - `/Library/TeX/texbin/pdflatex -interaction=nonstopmode -output-directory=. '\pdfcompresslevel=0 \pdfobjcompresslevel=0 \input{../<name>.tex}'`
  - Each fixture compiled twice to resolve section references, tables of contents, and appendix numbering

## Produced references

- `abstract_keywords.tex` -> `abstract_keywords.pdf`
- `article_with_appendix.tex` -> `article_with_appendix.pdf`
- `deep_sections.tex` -> `deep_sections.pdf`
- `long_article.tex` -> `long_article.pdf`
- `multicol_article.tex` -> `multicol_article.pdf`
- `sections_crossref.tex` -> `sections_crossref.pdf`
- `sections_independent.tex` -> `sections_independent.pdf`
- `sections_mixed.tex` -> `sections_mixed.pdf`

## Notes

- `\pdfcompresslevel=0` disables page-stream compression so section-oriented page content remains directly inspectable.
- `\pdfobjcompresslevel=0` keeps catalog and page-tree objects readable without object-stream decompression.
- Existing `sections_*.tex` fixtures that use `\makeatletter` compiled successfully without extra handling under the same two-pass command.

## Parity gate scope

- Full compile coverage remains on all 8 fixtures via `corpus_partition_article_cases()` and `corpus_partition_article_documents_compile_successfully`.
- REQ-NF-007 parity gate uses the representative subset `article_with_appendix.tex`, `abstract_keywords.tex`, and `deep_sections.tex`.
- The representative subset totals 83 source lines, which keeps parity execution within the CI time budget while still covering representative article partition behavior, including appendix transitions, front-matter metadata, and deeper nesting.
- The subset intentionally avoids benchmark-only loop fixtures so parity failures reflect article partition behavior rather than synthetic throughput workloads.
