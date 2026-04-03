# tikz/nested-style-transform-clip-arrow reference PDFs

- Generated with: `/Library/TeX/texbin/pdflatex`
- pdfLaTeX version:
  - `pdfTeX 3.141592653-2.6-1.40.28 (TeX Live 2025)`
  - `kpathsea version 6.4.1`
- Date generated: `2026-04-02 11:46 JST`
- Working directory: `ferritex/crates/ferritex-bench/fixtures/corpus/tikz/nested-style-transform-clip-arrow/reference/`
- Exact generation command used:
- `/Library/TeX/texbin/pdflatex -interaction=nonstopmode -jobname=<name> -output-directory=crates/ferritex-bench/fixtures/corpus/tikz/nested-style-transform-clip-arrow/reference '\RequirePackage{tikz}\pdfcompresslevel=0 \pdfobjcompresslevel=0 \input{crates/ferritex-bench/fixtures/corpus/tikz/nested-style-transform-clip-arrow/<name>.tex}'`

## Produced references

- `arrow_styles.tex` -> `arrow_styles.pdf`
- `clip_rect.tex` -> `clip_rect.pdf`
- `combined_nested.tex` -> `combined_nested.pdf`
- `scope_style_inherit.tex` -> `scope_style_inherit.pdf`
- `transform_shift.tex` -> `transform_shift.pdf`
- `rotate_scale.tex` -> `rotate_scale.pdf`
- `foreach_loop.tex` -> `foreach_loop.pdf`
- `layered_drawing.tex` -> `layered_drawing.pdf`

## Notes

- `\pdfcompresslevel=0` is required so the page content streams stay uncompressed and the parity harness can inspect graphics operators without decompression.
- `\pdfobjcompresslevel=0` keeps PDF objects out of `/ObjStm`, matching the other corpus subsets and keeping operator/resource dictionaries directly readable.
- The fixture sources do not declare `\usepackage{tikz}`, so the command preloads TikZ with `\RequirePackage{tikz}` before `\input{...}` to obtain valid pdfLaTeX references.
