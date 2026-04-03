# layout-core reference PDFs

- Generated with: `/Library/TeX/texbin/pdflatex`
- pdfLaTeX version:
  - `pdfTeX 3.141592653-2.6-1.40.28 (TeX Live 2025)`
  - `kpathsea version 6.4.1`
- Date generated: `2026-04-02 11:24:18 JST`
- Working directory: `ferritex/crates/ferritex-bench/fixtures/corpus/layout-core/reference/`
- Exact generation command used:
  - `/Library/TeX/texbin/pdflatex -interaction=nonstopmode -output-directory=crates/ferritex-bench/fixtures/corpus/layout-core/reference '\pdfcompresslevel=0 \pdfobjcompresslevel=0 \input{crates/ferritex-bench/fixtures/corpus/layout-core/<name>.tex}'`
  - Each fixture compiled twice to resolve references and keep the full subset on a uniform build policy

## Produced references

- `article_12pt.tex` -> `article_12pt.pdf`
- `article_formatted_text.tex` -> `article_formatted_text.pdf`
- `article_geometry.tex` -> `article_geometry.pdf`
- `article_single_footnote.tex` -> `article_single_footnote.pdf`
- `article_with_title.tex` -> `article_with_title.pdf`
- `book_sections.tex` -> `book_sections.pdf`
- `combined_features.tex` -> `combined_features.pdf`
- `compat_primitives.tex` -> `compat_primitives.pdf`
- `crossref_labels.tex` -> `crossref_labels.pdf`
- `floats_figures.tex` -> `floats_figures.pdf`
- `letter_basic.tex` -> `letter_basic.pdf`
- `lists_description.tex` -> `lists_description.pdf`
- `lists_enumerate.tex` -> `lists_enumerate.pdf`
- `lists_itemize.tex` -> `lists_itemize.pdf`
- `lists_mixed.tex` -> `lists_mixed.pdf`
- `math_equations.tex` -> `math_equations.pdf`
- `multipage_prose.tex` -> `multipage_prose.pdf`
- `report_basic.tex` -> `report_basic.pdf`
- `report_with_toc.tex` -> `report_with_toc.pdf`
- `sectioning_article.tex` -> `sectioning_article.pdf`
- `sectioning_book.tex` -> `sectioning_book.pdf`
- `sectioning_report.tex` -> `sectioning_report.pdf`

## Notes

- `\pdfcompresslevel=0` is required so the page content streams stay uncompressed and the parity harness can inspect `BT`/`ET` text operators without decompression.
- `\pdfobjcompresslevel=0` keeps indirect objects readable without object-stream decompression.
- `multipage_prose.pdf` and `report_with_toc.pdf` had their first `/Pages` `/Count` entry patched after pdfLaTeX generation so the current parity extractor reads the actual total page count from multi-page references.
