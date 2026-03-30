# layout-core reference PDFs

- Generated with: `/Library/TeX/texbin/pdflatex`
- pdfLaTeX version:
  - `pdfTeX 3.141592653-2.6-1.40.28 (TeX Live 2025)`
  - `kpathsea version 6.4.1`
- Date generated: `2026-03-30 18:55:04 JST`
- Working directory: `ferritex/crates/ferritex-bench/fixtures/corpus/layout-core/reference/`
- Exact generation command used:
  - `/Library/TeX/texbin/pdflatex -interaction=nonstopmode -output-directory=. '\pdfcompresslevel=0 \input{../<name>.tex}'`

## Produced references

- `combined_features.tex` -> `combined_features.pdf`
- `compat_primitives.tex` -> `compat_primitives.pdf`
- `crossref_labels.tex` -> `crossref_labels.pdf`
- `floats_figures.tex` -> `floats_figures.pdf`
- `letter_basic.tex` -> `letter_basic.pdf`
- `lists_description.tex` -> `lists_description.pdf`
- `lists_enumerate.tex` -> `lists_enumerate.pdf`
- `lists_itemize.tex` -> `lists_itemize.pdf`
- `math_equations.tex` -> `math_equations.pdf`
- `sectioning_article.tex` -> `sectioning_article.pdf`
- `sectioning_book.tex` -> `sectioning_book.pdf`
- `sectioning_report.tex` -> `sectioning_report.pdf`

## Notes

- `\pdfcompresslevel=0` is required so the page content streams stay uncompressed and the parity harness can inspect `BT`/`ET` text operators without decompression.
