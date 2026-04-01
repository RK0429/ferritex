# tikz/basic-shapes reference PDFs

- Generated with: `/Library/TeX/texbin/pdflatex`
- pdfLaTeX version:
  - `pdfTeX 3.141592653-2.6-1.40.28 (TeX Live 2025)`
  - `kpathsea version 6.4.1`
- Date generated: `2026-04-02 05:08:39 JST`
- Working directory: `ferritex/crates/ferritex-bench/fixtures/corpus/tikz/basic-shapes/reference/`
- Exact generation command used:
  - `/Library/TeX/texbin/pdflatex -interaction=nonstopmode -jobname=<name> -output-directory=crates/ferritex-bench/fixtures/corpus/tikz/basic-shapes/reference '\RequirePackage{tikz}\pdfcompresslevel=0 \input{crates/ferritex-bench/fixtures/corpus/tikz/basic-shapes/<name>.tex}'`

## Produced references

- `circle.tex` -> `circle.pdf`
- `line.tex` -> `line.pdf`
- `mixed_shapes.tex` -> `mixed_shapes.pdf`
- `rectangle.tex` -> `rectangle.pdf`
- `text_node.tex` -> `text_node.pdf`

## Notes

- `\pdfcompresslevel=0` is required so the page content streams stay uncompressed and the parity harness can inspect graphics operators without decompression.
- The fixture sources do not declare `\usepackage{tikz}`, so the command preloads TikZ with `\RequirePackage{tikz}` before `\input{...}` to obtain valid pdfLaTeX references.
