# embedded-assets reference PDFs

- Generated with: `/Library/TeX/texbin/pdflatex`
- pdfLaTeX version:
  - `pdfTeX 3.141592653-2.6-1.40.28 (TeX Live 2025)`
  - `kpathsea version 6.4.1`
- Date generated: `2026-04-02 08:35 JST`
- Working directory: `ferritex/crates/ferritex-bench/fixtures/corpus/embedded-assets/reference/`
- Exact generation commands used:
  - `cp ../pixel.png ./pixel.png`
  - `cp ../diagram.pdf ./diagram.pdf`
  - `/Library/TeX/texbin/pdflatex -interaction=nonstopmode -output-directory=. '\pdfcompresslevel=0 \pdfobjcompresslevel=0 \input{../png_embed.tex}'`
  - `/Library/TeX/texbin/pdflatex -interaction=nonstopmode -output-directory=. '\pdfcompresslevel=0 \pdfobjcompresslevel=0 \input{../pdf_embed.tex}'`
  - `/Library/TeX/texbin/pdflatex -interaction=nonstopmode -output-directory=. '\pdfcompresslevel=0 \pdfobjcompresslevel=0 \input{../scaled_image.tex}'`
  - `/Library/TeX/texbin/pdflatex -interaction=nonstopmode -output-directory=. '\pdfcompresslevel=0 \pdfobjcompresslevel=0 \input{../figure_with_caption.tex}'`
  - `/Library/TeX/texbin/pdflatex -interaction=nonstopmode -output-directory=. '\pdfcompresslevel=0 \pdfobjcompresslevel=0 \input{../figure_with_caption.tex}'`
  - `/Library/TeX/texbin/pdflatex -interaction=nonstopmode -output-directory=. '\pdfcompresslevel=0 \pdfobjcompresslevel=0 \input{../mixed_embeds.tex}'`

## Produced references

- `png_embed.tex` -> `png_embed.pdf`
- `pdf_embed.tex` -> `pdf_embed.pdf`
- `scaled_image.tex` -> `scaled_image.pdf`
- `figure_with_caption.tex` -> `figure_with_caption.pdf`
- `mixed_embeds.tex` -> `mixed_embeds.pdf`

## Notes

- `\pdfcompresslevel=0` disables content stream compression so the parity harness can inspect embedded-asset resources without decompression.
- `\pdfobjcompresslevel=0` disables object stream compression so font and XObject dictionaries remain directly readable by the PDF comparators.
- The sibling assets `pixel.png` and `diagram.pdf` were copied into `reference/` before compilation and removed after PDF generation so `\includegraphics` resolves relative paths exactly as it does in the staged parity harness.
- `figure_with_caption.tex` was compiled twice to resolve `\ref`/`\label` cross-references before recording the reference PDF.
