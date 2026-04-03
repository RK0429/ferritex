# combined-stress reference PDFs

- Generated with: `/Library/TeX/texbin/pdflatex`
- pdfLaTeX version:
  - `pdfTeX 3.141592653-2.6-1.40.28 (TeX Live 2025)`
  - `kpathsea version 6.4.1`
- Date generated: `2026-04-02 10:35:33 JST`
- Working directory: `ferritex/crates/ferritex-bench/fixtures/corpus/combined-stress/reference/`
- Exact generation command used:
  - `/Library/TeX/texbin/pdflatex -interaction=nonstopmode -output-directory=. '\pdfcompresslevel=0 \pdfobjcompresslevel=0 \input{../<name>.tex}'`

## Produced references

- `article_full_features.tex` -> `article_full_features.pdf`
- `book_full.tex` -> `book_full.pdf`
- `hyperref_with_bib.tex` -> `hyperref_with_bib.pdf`
- `letter_extended.tex` -> `letter_extended.pdf`
- `long_report.tex` -> `long_report.pdf`
- `math_heavy.tex` -> `math_heavy.pdf`
- `report_comprehensive.tex` -> `report_comprehensive.pdf`
- `stress_50_pages.tex` -> `stress_50_pages.pdf`
- `tables_and_figures.tex` -> `tables_and_figures.pdf`
- `twocolumn_academic.tex` -> `twocolumn_academic.pdf`

## Notes

- All documents were compiled twice so table-of-contents entries, cross-references, and bibliography citations settled before the reference PDFs were kept.
- The combined-stress subset is self-contained: placeholder figures are built from `\rule`, and no external image files are required.
- `long_report.pdf` produced 23 pages and `stress_50_pages.pdf` produced 50 pages when generated with the command above.

## Parity gate scope

- Full compile coverage remains on all 10 fixtures via `corpus_combined_stress_cases()` and `corpus_combined_stress_documents_compile_successfully`.
- REQ-NF-007 parity gate uses the representative subset `article_full_features.tex`, `letter_extended.tex`, and `twocolumn_academic.tex`.
- The representative subset totals about 130 source lines, which keeps parity execution within the CI time budget while still covering dense article features, extended letter layout, and two-column stress behavior.
