# navigation-features reference PDFs

- Generated with: `/Library/TeX/texbin/pdflatex`
- pdfLaTeX version:
  - `pdfTeX 3.141592653-2.6-1.40.28 (TeX Live 2025)`
  - `kpathsea version 6.4.1`
- Date generated: `2026-04-02 06:15 JST`
- Working directory: `ferritex/crates/ferritex-bench/fixtures/corpus/navigation-features/reference/`
- Exact generation command used:
  - `/Library/TeX/texbin/pdflatex -interaction=nonstopmode -output-directory=. '\pdfcompresslevel=0 \pdfobjcompresslevel=0 \input{../<name>.tex}'`
  - Each fixture compiled twice to resolve cross-references and outlines

## Produced references

- `book_outlines.tex` -> `book_outlines.pdf`
- `colorlinks.tex` -> `colorlinks.pdf`
- `external_links.tex` -> `external_links.pdf`
- `hyperref_basic.tex` -> `hyperref_basic.pdf`
- `internal_links.tex` -> `internal_links.pdf`
- `mixed_navigation.tex` -> `mixed_navigation.pdf`
- `outlines_sections.tex` -> `outlines_sections.pdf`
- `pdf_metadata.tex` -> `pdf_metadata.pdf`

## Notes

- `\pdfcompresslevel=0` disables content stream compression so the parity harness can inspect PDF structures without decompression.
- `\pdfobjcompresslevel=0` disables object stream compression so annotations, destinations, outlines, and metadata dict objects are directly accessible without decompressing ObjStm.
- All fixtures load `\usepackage{hyperref}` to produce navigation features (annotations, destinations, outlines, metadata) that the navigation parity comparator can verify.
