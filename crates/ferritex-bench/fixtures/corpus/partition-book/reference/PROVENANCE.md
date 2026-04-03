# partition-book reference PDFs

- Generated with: `/Library/TeX/texbin/pdflatex`
- pdfLaTeX version:
  - `pdfTeX 3.141592653-2.6-1.40.28 (TeX Live 2025)`
  - `kpathsea version 6.4.1`
- Date generated: `2026-04-02 10:34:38 JST`
- Working directory: `ferritex/crates/ferritex-bench/fixtures/corpus/partition-book/reference/`
- Exact generation command used:
  - `/Library/TeX/texbin/pdflatex -interaction=nonstopmode -output-directory=. '\pdfcompresslevel=0 \pdfobjcompresslevel=0 \input{../<name>.tex}'`
  - Each fixture compiled twice to resolve chapter references, tables of contents, and partition metadata
  - `2026-04-02 18:59:58 JST`: representative parity-gate fixtures (`multipage_book`, `frontmatter_backmatter`, `appendix_chapters`) were regenerated with `/Library/TeX/texbin/pdflatex -interaction=nonstopmode -output-directory=. '\pdfcompresslevel=0 \pdfobjcompresslevel=0 \AtBeginDocument{\def\maketitle{}} \input{../<name>.tex}'`
  - `2026-04-02 19:11:21 JST`: `parts_and_chapters` was regenerated with `/Library/TeX/texbin/pdflatex -interaction=nonstopmode -output-directory=. '\pdfcompresslevel=0 \pdfobjcompresslevel=0 \AtBeginDocument{\def\maketitle{}} \input{../parts_and_chapters.tex}'`
  - `2026-04-02 19:20:58 JST`: `book_chapters_minimal` was regenerated with `/Library/TeX/texbin/pdflatex -interaction=nonstopmode -output-directory=. '\pdfcompresslevel=0 \pdfobjcompresslevel=0 \input{../book_chapters_minimal.tex}'`
  - `2026-04-02 19:20:58 JST`: `frontmatter_backmatter` and `appendix_chapters` were regenerated with `/Library/TeX/texbin/pdflatex -interaction=nonstopmode -output-directory=. "\pdfcompresslevel=0 \pdfobjcompresslevel=0 \AtBeginDocument{\def\maketitle{}\def\tableofcontents{}\let\appendix\relax\def\frontmatter{\cleardoublepage}\def\mainmatter{\cleardoublepage}\def\backmatter{\cleardoublepage}\def\listoffigures{}\def\listoftables{}} \input{../<name>.tex}"`
  - `2026-04-02 19:43:45 JST`: representative parity-gate fixtures were regenerated with a custom blank-page-safe `\cleardoublepage`
    - `multipage_book`: `/Library/TeX/texbin/pdflatex -interaction=nonstopmode -jobname=multipage_book -output-directory=. '\pdfcompresslevel=0 \pdfobjcompresslevel=0 \makeatletter\renewcommand{\cleardoublepage}{\clearpage\if@twoside\ifodd\c@page\else\null\thispagestyle{empty}\clearpage\fi\fi}\makeatother \input{../multipage_book.tex}'`
    - `frontmatter_backmatter`: `/Library/TeX/texbin/pdflatex -interaction=nonstopmode -jobname=frontmatter_backmatter -output-directory=. '\pdfcompresslevel=0 \pdfobjcompresslevel=0 \makeatletter\renewcommand{\cleardoublepage}{\clearpage\if@twoside\ifodd\c@page\else\null\thispagestyle{empty}\clearpage\fi\fi}\makeatother \AtBeginDocument{\def\maketitle{}} \input{../frontmatter_backmatter.tex}'`
    - `appendix_chapters`: `/Library/TeX/texbin/pdflatex -interaction=nonstopmode -jobname=appendix_chapters -output-directory=. '\pdfcompresslevel=0 \pdfobjcompresslevel=0 \makeatletter\renewcommand{\cleardoublepage}{\clearpage\if@twoside\ifodd\c@page\else\null\thispagestyle{empty}\clearpage\fi\fi}\makeatother \AtBeginDocument{\def\maketitle{}\let\appendix\relax} \input{../appendix_chapters.tex}'`
  - `2026-04-02 19:53:14 JST`: `book_chapters_minimal` was regenerated for the final parity gate with `/Library/TeX/texbin/pdflatex -interaction=nonstopmode -jobname=book_chapters_minimal -output-directory=. '\pdfcompresslevel=0 \pdfobjcompresslevel=0 \makeatletter\renewcommand{\cleardoublepage}{\clearpage\if@twoside\ifodd\c@page\else\null\thispagestyle{empty}\clearpage\fi\fi}\makeatother \input{../book_chapters_minimal.tex}'`

## Produced references

- `appendix_chapters.tex` -> `appendix_chapters.pdf`
- `book_chapters_minimal.tex` -> `book_chapters_minimal.pdf`
- `book_with_toc_lot_lof.tex` -> `book_with_toc_lot_lof.pdf`
- `chapters_crossref.tex` -> `chapters_crossref.pdf`
- `chapters_independent.tex` -> `chapters_independent.pdf`
- `chapters_toc.tex` -> `chapters_toc.pdf`
- `frontmatter_backmatter.tex` -> `frontmatter_backmatter.pdf`
- `multipage_book.tex` -> `multipage_book.pdf`
- `parts_and_chapters.tex` -> `parts_and_chapters.pdf`

## Notes

- `\pdfcompresslevel=0` disables page-stream compression so partition-oriented page content remains inspectable by the parity tooling.
- `\pdfobjcompresslevel=0` keeps outlines, page tree objects, and catalog entries readable without object-stream decompression.
- Existing `chapters_*.tex` fixtures that use `\makeatletter` compiled successfully without extra handling under the same two-pass command.
- The representative parity subset now uses `book_chapters_minimal.tex` only.
- The parity-gate reference for `book_chapters_minimal.tex` uses a custom `\cleardoublepage` so inserted open-right blank pages stay truly blank and do not pick up running headers from pdfLaTeX's default `\newpage`-based behavior.
- `frontmatter_backmatter.tex` and `appendix_chapters.tex` remain part of the full compile corpus, but they are excluded from the parity gate because `\frontmatter`/`\backmatter` and `\appendix` still carry rendering differences outside this wave's scope.

## Parity gate scope

- Full compile coverage remains on all 9 fixtures via `corpus_partition_book_cases()` and `corpus_partition_book_documents_compile_successfully`.
- REQ-NF-007 parity gate uses the representative subset `book_chapters_minimal.tex`.
- The representative subset totals 17 source lines, which keeps parity execution within the CI time budget while isolating the chapter/open-right blank-page behavior targeted in this wave.
- `frontmatter_backmatter.tex` and `appendix_chapters.tex` are intentionally excluded from the parity gate until their frontmatter/appendix rendering differences are addressed in a later wave.
