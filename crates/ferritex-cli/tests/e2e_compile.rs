use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const PNG_1X1_RGB: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0,
    0, 0, 144, 119, 83, 222, 0, 0, 0, 12, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 0, 0, 3, 1,
    1, 0, 201, 254, 146, 239, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

fn ferritex_bin() -> Command {
    let bin = env!("CARGO_BIN_EXE_ferritex");
    Command::new(bin)
}

#[test]
fn compile_nonexistent_file_exits_with_code_2() {
    let output = ferritex_bin()
        .args(["compile", "nonexistent.tex"])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("input file not found"));
}

#[test]
fn compile_existing_file_writes_pdf_with_document_content() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("hello.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\nHello, Ferritex!\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.trim().is_empty());

    let pdf_file = dir.path().join("hello.pdf");
    let pdf = std::fs::read_to_string(&pdf_file).expect("read output pdf");
    assert!(pdf.starts_with("%PDF-1.4"));
    assert!(pdf.contains("Hello, Ferritex!"));
    assert!(!pdf.contains("Ferritex placeholder PDF"));
}

#[test]
fn compile_with_trace_font_tasks_emits_font_task_trace_to_stderr() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("trace-fonts.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\nTrace me\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args([
            "compile",
            "--trace-font-tasks",
            tex_file.to_str().expect("utf-8 path"),
        ])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("\"fontTaskId\""));
    assert!(stderr.contains("\"fontAsset\""));
    assert!(stderr.contains("\"startedAt\""));
    assert!(stderr.contains("\"finishedAt\""));
    assert!(stderr.contains("\"workerId\""));
    assert!(!stderr.contains("error:"));
}

#[test]
fn compile_includegraphics_embeds_image_xobject_into_pdf() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("image.tex");
    let image_file = dir.path().join("pixel.png");
    std::fs::write(&image_file, PNG_1X1_RGB).expect("write image file");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\nBefore\n\\includegraphics[width=100pt]{pixel.png}\nAfter\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));

    let pdf = std::fs::read(dir.path().join("image.pdf")).expect("read output pdf");
    let content = String::from_utf8_lossy(&pdf);
    assert!(content.starts_with("%PDF-1.4"));
    assert!(content.contains("/Subtype /Image"));
    assert!(content.contains("/Filter /FlateDecode"));
    assert!(content.contains("/XObject << /Im1"));
    assert!(content.contains("/Im1 Do"));
}

#[test]
fn compile_figure_environment_renders_inline_caption_and_ref() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("figure.tex");
    let image_file = dir.path().join("pixel.png");
    std::fs::write(&image_file, PNG_1X1_RGB).expect("write image file");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\nSee Figure \\ref{fig:pixel}.\n\\begin{figure}[h]\n\\includegraphics[width=100pt]{pixel.png}\n\\caption{Embedded pixel}\n\\label{fig:pixel}\n\\end{figure}\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));

    let pdf = std::fs::read(dir.path().join("figure.pdf")).expect("read output pdf");
    let content = String::from_utf8_lossy(&pdf);
    assert!(content.contains("See Figure 1."));
    assert!(content.contains("Figure 1: Embedded pixel"));
    assert!(content.contains("/Subtype /Image"));
    assert!(content.contains("/Im1 Do"));
    assert!(!content.contains("??"));
}

#[test]
fn compile_renders_inline_and_display_math_without_raw_tex_delimiters() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("math.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\nInline $x^2$ here.\n\\[\\frac{a}{b}\\]\nDone.\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));

    let pdf = std::fs::read_to_string(dir.path().join("math.pdf")).expect("read output pdf");
    assert!(pdf.contains("Inline x2 here."));
    assert!(pdf.contains("a/b"));
    assert!(pdf.contains("Done."));
    assert!(!pdf.contains("$x^2$"));
    assert!(!pdf.contains("\\frac{a}{b}"));
    assert!(!pdf.contains("\\["));
}

#[test]
fn compile_renders_multiline_math_environments_with_tags_and_text() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("multiline-math.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\n\\begin{equation}\nE=mc^2\\label{eq:e}\n\\end{equation}\nRef \\ref{eq:e}.\n\\begin{equation*}\nx=y\n\\end{equation*}\n\\begin{align}\na&=&b\\notag\\\\\nc&=&\\text{done}\\tag{A}\\label{eq:done}\n\\end{align}\nAlso \\ref{eq:done}.\n\\begin{align*}\nu&=&v\\\\\nw&=&z\n\\end{align*}\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));

    let pdf =
        std::fs::read_to_string(dir.path().join("multiline-math.pdf")).expect("read output pdf");
    assert!(pdf.contains("E=mc2 \\(1\\)"));
    assert!(pdf.contains("Ref 1."));
    assert!(pdf.contains("x=y"));
    assert!(!pdf.contains("x=y \\("));
    assert!(pdf.contains("a=b"));
    assert!(!pdf.contains("a=b \\("));
    assert!(pdf.contains("c=done \\(A\\)"));
    assert!(pdf.contains("Also A."));
    assert!(pdf.contains("u=v"));
    assert!(pdf.contains("w=z"));
    assert!(!pdf.contains("\\tag{A}"));
    assert!(!pdf.contains("\\text{done}"));
}

#[test]
fn compile_renders_equation_environment_with_numbering() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("equation.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\n\\begin{equation}\nx^2 + y^2 = z^2\n\\end{equation}\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));
    let pdf = std::fs::read_to_string(dir.path().join("equation.pdf")).expect("read output pdf");
    assert!(pdf.contains("x2+y2=z2"));
    assert!(pdf.contains("\\(1\\)"));
}

#[test]
fn compile_renders_equation_star_without_numbering() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("eqstar.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\n\\begin{equation*}\na + b = c\n\\end{equation*}\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));
    let pdf = std::fs::read_to_string(dir.path().join("eqstar.pdf")).expect("read output pdf");
    assert!(pdf.contains("a+b=c"));
    assert!(!pdf.contains("\\(1\\)"));
}

#[test]
fn compile_renders_align_environment_with_numbering() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("align.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\n\\begin{align}\na &= b \\\\\nc &= d\n\\end{align}\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));
    let pdf = std::fs::read_to_string(dir.path().join("align.pdf")).expect("read output pdf");
    assert!(pdf.contains("a=b"));
    assert!(pdf.contains("c=d"));
    assert!(pdf.contains("\\(1\\)"));
    assert!(pdf.contains("\\(2\\)"));
}

#[test]
fn compile_renders_notag_and_tag_in_align_environment() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("tags.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\n\\begin{align}\na &= b \\notag \\\\\nc &= d \\tag{*}\n\\end{align}\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));
    let pdf = std::fs::read_to_string(dir.path().join("tags.pdf")).expect("read output pdf");
    assert!(pdf.contains("a=b"));
    assert!(pdf.contains("c=d"));
    assert!(!pdf.contains("\\(1\\)"));
    assert!(pdf.contains("\\(*\\)"));
}

#[test]
fn compile_long_paragraph_produces_multi_page_pdf() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("long.tex");
    let first_word = "Alpha".repeat(14);
    let filler_word = "BetaX".repeat(14);
    let last_word = "Omega".repeat(14);
    let paragraph = std::iter::once(first_word.as_str())
        .chain((0..58).map(|_| filler_word.as_str()))
        .chain(std::iter::once(last_word.as_str()))
        .collect::<Vec<_>>()
        .join(" ");

    std::fs::write(
        &tex_file,
        format!(
            "\\documentclass{{article}}\n\\begin{{document}}\n{paragraph}\n\\end{{document}}\n"
        ),
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));

    let pdf = std::fs::read_to_string(dir.path().join("long.pdf")).expect("read output pdf");
    assert!(pdf.starts_with("%PDF-1.4"));
    assert!(pdf.contains(&first_word));
    assert!(pdf.contains(&last_word));
    assert!(!pdf.contains("Ferritex placeholder PDF"));
    assert!(pdf_page_count(&pdf) >= 2);
}

#[test]
fn compile_expands_def_macro_into_pdf_output() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("macro.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\def\\hello{Hello, Macro!}\n\\begin{document}\n\\hello\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));
    let pdf = std::fs::read_to_string(dir.path().join("macro.pdf")).expect("read output pdf");
    assert!(pdf.contains("Hello, Macro!"));
}

#[test]
fn compile_applies_catcode_changes_during_parsing() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("catcode.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\catcode`\\@=11\n\\def\\make@title{Catcode parsing works}\n\\begin{document}\n\\make@title\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));
    let pdf = std::fs::read_to_string(dir.path().join("catcode.pdf")).expect("read output pdf");
    assert!(pdf.contains("Catcode parsing works"));
}

#[test]
fn compile_respects_group_scoped_macros() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("scoped.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\n{\\def\\local{Scoped }\\local}\\local\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));
    let pdf = std::fs::read_to_string(dir.path().join("scoped.pdf")).expect("read output pdf");
    assert!(pdf.contains("Scoped "));
    assert!(pdf.contains("\\\\local"));
}

#[test]
fn compile_handles_conditionals_and_registers() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("conditionals.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\n\\count0 42\\advance\\count0 8\\dimen0 2pt\\iftrue Visible \\the\\count0\\fi\\ifnum\\count0>40 Positive \\the\\dimen0\\fi\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));
    let pdf =
        std::fs::read_to_string(dir.path().join("conditionals.pdf")).expect("read output pdf");
    assert!(pdf.contains("Visible 50"));
    assert!(pdf.contains("Positive"));
    assert!(pdf.contains("2.0pt"));
}

#[test]
fn compile_handles_ifdim_and_ifcase() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("ifdim-ifcase.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\n\\ifdim1pt<2pt Smaller\\else Larger\\fi\n\\ifcase1 zero\\or one\\or two\\fi\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));
    let pdf =
        std::fs::read_to_string(dir.path().join("ifdim-ifcase.pdf")).expect("read output pdf");
    assert!(pdf.contains("Smaller"));
    assert!(pdf.contains("one"));
}

#[test]
fn compile_renders_itemize_environment_as_bulleted_text() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("itemize.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\n\\begin{itemize}\n\\item First item\n\\item Second item\n\\end{itemize}\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));
    let pdf = std::fs::read_to_string(dir.path().join("itemize.pdf")).expect("read output pdf");
    assert!(pdf.contains("• First item"));
    assert!(pdf.contains("• Second item"));
}

#[test]
fn compile_renders_tableofcontents_from_second_pass_entries() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("toc.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\n\\tableofcontents\n\\section{Intro}\n\\subsection{Scope}\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));
    let pdf = std::fs::read_to_string(dir.path().join("toc.pdf")).expect("read output pdf");
    assert!(pdf.match_indices("1 Intro").count() >= 2);
    assert!(pdf.match_indices("1.1 Scope").count() >= 2);
    assert!(!pdf.contains("??"));
}

#[test]
fn compile_renders_lists_of_figures_and_tables_from_second_pass_entries() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("lists.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\n\\listoffigures\n\\listoftables\n\\begin{figure}\\caption{Embedded pixel}\\end{figure}\n\\begin{table}\\caption{Metrics}\\end{table}\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));
    let pdf = std::fs::read_to_string(dir.path().join("lists.pdf")).expect("read output pdf");
    assert!(pdf.match_indices("Figure 1: Embedded pixel").count() >= 2);
    assert!(pdf.match_indices("Table 1: Metrics").count() >= 2);
    assert!(!pdf.contains("??"));
}

#[test]
fn compile_emits_citations_links_outlines_and_metadata() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("wave7.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\title{Ferritex Wave 7}\n\\author{Ada Lovelace}\n\\begin{document}\n\\section{Intro}\n\\subsection{Links}\nSee \\href{https://example.com}{Example Site} and \\url{https://openai.com}.\nReference \\cite{knuth}.\n\\begin{thebibliography}{99}\n\\bibitem{knuth} Donald Knuth.\n\\end{thebibliography}\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));

    let pdf = std::fs::read_to_string(dir.path().join("wave7.pdf")).expect("read output pdf");
    assert!(pdf.contains("Reference [1]."));
    assert!(!pdf.contains("[?]"));
    assert!(pdf.contains("/Subtype /Link"));
    assert!(pdf.contains("/URI (https://example.com)"));
    assert!(pdf.contains("/URI (https://openai.com)"));
    assert!(pdf.contains("/Outlines"));
    assert!(pdf.contains("/Title (1 Intro)"));
    assert!(pdf.contains("/Title (1.1 Links)"));
    assert!(pdf.contains("/Author (Ada Lovelace)"));
    assert!(pdf.contains("/Title (Ferritex Wave 7)"));
}

#[test]
fn compile_emits_internal_navigation_annotations_and_named_destinations() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("hyperref-internal.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\usepackage{hyperref}\n\\begin{document}\n\\tableofcontents\n\\section{Intro}\\label{sec:intro}\nSee \\hyperref[sec:intro]{intro}, Section \\ref{sec:intro}, page \\pageref{sec:intro}, and citation \\cite{knuth}.\nExternal \\href{https://example.com}{Example}.\n\\begin{thebibliography}{99}\n\\bibitem{knuth} Donald Knuth.\n\\end{thebibliography}\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));

    let pdf =
        std::fs::read_to_string(dir.path().join("hyperref-internal.pdf")).expect("read output pdf");
    assert!(pdf.contains("/Subtype /Link"));
    assert!(pdf.matches("/S /GoTo /D (sec:intro)").count() >= 3);
    assert!(pdf.contains("/S /GoTo /D (bib:knuth)"));
    assert!(pdf.contains("/S /GoTo /D (section:1 Intro)"));
    assert!(pdf.contains("/Names << /Dests"));
    assert!(pdf.contains("(sec:intro) ["));
    assert!(pdf.contains("(bib:knuth) ["));
    assert!(pdf.contains("(section:1 Intro) ["));
    assert!(pdf.contains("/URI (https://example.com)"));
    assert!(pdf.contains("/Outlines"));
    assert!(!pdf.contains("??"));
}

#[test]
fn compile_hypersetup_overrides_pdf_metadata() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("hypersetup.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\title{Visible Title}\n\\author{Visible Author}\n\\hypersetup{pdftitle={Test Title},pdfauthor={Test Author},colorlinks=true}\n\\begin{document}\nSee \\href{https://example.com}{Example}.\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));

    let pdf = std::fs::read_to_string(dir.path().join("hypersetup.pdf")).expect("read output pdf");
    assert!(pdf.contains("/Title (Test Title)"));
    assert!(pdf.contains("/Author (Test Author)"));
    assert!(!pdf.contains("/Title (Visible Title)"));
    assert!(pdf.contains("/Subtype /Link"));
    assert!(pdf.contains("/URI (https://example.com)"));
}

#[test]
fn compile_colorlinks_true_renders_colored_link_text() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("colorlinks-true.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\hypersetup{colorlinks=true,linkcolor=red}\n\\begin{document}\n\\href{https://example.com}{click}\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));

    let pdf =
        std::fs::read_to_string(dir.path().join("colorlinks-true.pdf")).expect("read output pdf");
    assert!(pdf.contains("1 0 0 rg"));
    assert!(pdf.contains("(click) Tj"));
    assert!(pdf.contains("/Subtype /Link"));
    assert!(pdf.contains("/URI (https://example.com)"));
    assert!(pdf.contains("/Border [0 0 0]"));
}

#[test]
fn compile_colorlinks_false_renders_visible_border() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("colorlinks-false.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\n\\href{https://example.com}{click}\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));

    let pdf =
        std::fs::read_to_string(dir.path().join("colorlinks-false.pdf")).expect("read output pdf");
    assert!(pdf.contains("/Subtype /Link"));
    assert!(pdf.contains("/URI (https://example.com)"));
    assert!(pdf.contains("/Border [0 0 1] /C [0 0 1]"));
    assert!(!pdf.contains("1 0 0 rg"));
}

#[test]
fn compile_expands_newenvironment_usage_into_pdf_output() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("environment.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\newenvironment{boxenv}{BEGIN }{ END}\n\\begin{document}\n\\begin{boxenv}content\\end{boxenv}\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));
    let pdf = std::fs::read_to_string(dir.path().join("environment.pdf")).expect("read output pdf");
    assert!(pdf.contains("BEGIN content END"));
}

#[test]
fn compile_resolves_nested_input_files() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let main = dir.path().join("main.tex");
    let chapter_dir = dir.path().join("chapters");
    let section_dir = chapter_dir.join("sections");
    std::fs::create_dir_all(&section_dir).expect("create source tree");
    std::fs::write(
        &main,
        "\\documentclass{article}\n\\begin{document}\n\\input{chapters/intro}\n\\end{document}\n",
    )
    .expect("write main");
    std::fs::write(
        chapter_dir.join("intro.tex"),
        "Intro line.\n\\input{sections/detail}\n",
    )
    .expect("write intro");
    std::fs::write(section_dir.join("detail.tex"), "Nested detail.\n").expect("write detail");

    let output = ferritex_bin()
        .args(["compile", main.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));
    let pdf = std::fs::read_to_string(dir.path().join("main.pdf")).expect("read output pdf");
    assert!(pdf.contains("Intro line."));
    assert!(pdf.contains("Nested detail."));
}

#[test]
fn compile_resolves_forward_refs_and_section_numbers_in_single_file() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("sections.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\nSee Section \\ref{sec:later}.\n\\section{Later}\\label{sec:later}\nMore text.\n\\subsection{Details}\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));
    let pdf = std::fs::read_to_string(dir.path().join("sections.pdf")).expect("read output pdf");
    assert!(pdf.contains("See Section 1."));
    assert!(pdf.contains("1 Later"));
    assert!(pdf.contains("1.1 Details"));
    assert!(!pdf.contains("??"));
}

#[test]
fn compile_resolves_refs_across_input_boundaries() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let main = dir.path().join("main.tex");
    let chapter = dir.path().join("chapter.tex");
    std::fs::write(
        &main,
        "\\documentclass{article}\n\\begin{document}\nMain sees input section \\ref{sec:input}.\n\\input{chapter}\n\\section{Main}\\label{sec:main}\nDone.\n\\end{document}\n",
    )
    .expect("write main");
    std::fs::write(
        &chapter,
        "\\section{Input}\\label{sec:input}\nInput sees main section \\ref{sec:main}.\n",
    )
    .expect("write chapter");

    let output = ferritex_bin()
        .args(["compile", main.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));
    let pdf = std::fs::read_to_string(dir.path().join("main.pdf")).expect("read output pdf");
    assert!(pdf.contains("Main sees input section 1."));
    assert!(pdf.contains("1 Input"));
    assert!(pdf.contains("Input sees main section 2."));
    assert!(pdf.contains("2 Main"));
    assert!(!pdf.contains("??"));
}

#[test]
fn compile_resolves_pageref_across_page_boundaries() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("pageref.tex");
    std::fs::write(
        &tex_file,
        format!(
            "\\documentclass{{article}}\n\\begin{{document}}\nSee page \\pageref{{sec:later}}.\n\\newpage\n\\section{{Later}}\\label{{sec:later}}\nDone.\n\\end{{document}}\n"
        ),
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));
    let pdf = std::fs::read_to_string(dir.path().join("pageref.pdf")).expect("read output pdf");
    assert!(pdf.contains("See page 2."));
    assert!(pdf.contains("1 Later"));
    assert!(!pdf.contains("??"));
}

#[test]
fn compile_index_resolves_page_numbers_across_passes() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("index.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\makeindex\n\\begin{document}\nAlpha\\index{Alpha}\n\\newpage\nBeta\\index{beta@Beta}\n\\printindex\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));

    let pdf = std::fs::read_to_string(dir.path().join("index.pdf")).expect("read output pdf");
    let alpha = pdf.find("Alpha . . . . 1").expect("Alpha index entry");
    let beta = pdf.find("Beta . . . . 2").expect("Beta index entry");
    assert!(alpha < beta);
    assert!(pdf.contains("A"));
    assert!(pdf.contains("B"));
    assert!(!pdf.contains("??"));
}

#[test]
fn compile_index_sort_at_display_syntax() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("index-display.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\makeindex\n\\begin{document}\nEntry\\index{sortkey@Display}\n\\printindex\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));

    let pdf =
        std::fs::read_to_string(dir.path().join("index-display.pdf")).expect("read output pdf");
    assert!(pdf.contains("Display . . . . 1"));
    assert!(!pdf.contains("sortkey . . . . 1"));
}

#[test]
fn compile_resolves_project_root_fallback_from_nested_input() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let project_root = dir.path().join("project");
    let src_dir = project_root.join("src");
    let section_dir = src_dir.join("chapters");
    let shared_dir = project_root.join("shared");
    std::fs::create_dir_all(&section_dir).expect("create source tree");
    std::fs::create_dir_all(&shared_dir).expect("create shared tree");

    std::fs::write(
        src_dir.join("main.tex"),
        "\\documentclass{article}\n\\begin{document}\n\\input{chapters/section}\n\\end{document}\n",
    )
    .expect("write main");
    std::fs::write(section_dir.join("section.tex"), "\\input{shared/macros}\n")
        .expect("write nested section");
    std::fs::write(shared_dir.join("macros.tex"), "Project root fallback.\n")
        .expect("write shared macros");

    let output = ferritex_bin()
        .current_dir(&project_root)
        .args(["compile", "src/main.tex"])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));
    let pdf = std::fs::read_to_string(src_dir.join("main.pdf")).expect("read output pdf");
    assert!(pdf.contains("Project root fallback."));
}

#[test]
fn compile_resolves_tex_input_from_asset_bundle_outside_project_root() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let project_root = dir.path().join("project");
    let bundle_root = dir.path().join("bundle");
    std::fs::create_dir_all(project_root.join("src")).expect("create source tree");
    std::fs::create_dir_all(bundle_root.join("texmf")).expect("create bundle texmf");
    std::fs::write(
        bundle_root.join("manifest.json"),
        r#"{"name":"default","version":"2026.03.18","min_ferritex_version":"0.1.0","format_version":1,"asset_index_path":"asset-index.json"}"#,
    )
    .expect("write bundle manifest");
    std::fs::write(
        bundle_root.join("asset-index.json"),
        r#"{"tex_inputs":{"bundled.tex":"texmf/bundled.tex"},"packages":{},"opentype_fonts":{},"tfm_fonts":{},"default_opentype_fonts":[]}"#,
    )
    .expect("write bundle asset index");
    std::fs::write(
        bundle_root.join("texmf/bundled.tex"),
        "Bundled from asset bundle.\n",
    )
    .expect("write bundled tex input");
    std::fs::write(
        project_root.join("src/main.tex"),
        "\\documentclass{article}\n\\begin{document}\n\\input{bundled}\n\\end{document}\n",
    )
    .expect("write main");

    let output = ferritex_bin()
        .current_dir(&project_root)
        .args([
            "compile",
            "src/main.tex",
            "--asset-bundle",
            bundle_root.to_str().expect("utf-8 bundle path"),
        ])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));
    let pdf = std::fs::read_to_string(project_root.join("src/main.pdf")).expect("read output pdf");
    assert!(pdf.contains("Bundled from asset bundle."));
}

#[test]
fn compile_accepts_builtin_asset_bundle_identifier() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("main.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\n\\input{bundled}\n\\end{document}\n",
    )
    .expect("write input");

    let output = ferritex_bin()
        .args([
            "compile",
            tex_file.to_str().expect("utf-8 path"),
            "--asset-bundle",
            "builtin:basic",
        ])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));
    let pdf = std::fs::read_to_string(dir.path().join("main.pdf")).expect("read output pdf");
    assert!(pdf.contains("Bundled from built-in asset bundle."));
}

#[test]
fn compile_rejects_commented_out_documentclass() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("broken.tex");
    std::fs::write(
        &tex_file,
        "% \\documentclass{article}\n\\begin{document}\nHello\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("missing \\documentclass declaration"));
    assert!(!dir.path().join("broken.pdf").exists());
}

#[test]
fn compile_rejects_trailing_content_after_end_document() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("trailing.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\nHello\n\\end{document}\nTrailing\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unexpected content after \\end{document}"));
    assert!(!dir.path().join("trailing.pdf").exists());
}

#[test]
fn compile_with_missing_asset_bundle_reports_validation_error() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("hello.tex");
    std::fs::write(&tex_file, "\\documentclass{article}\n").expect("write input file");

    let output = ferritex_bin()
        .args([
            "compile",
            tex_file.to_str().expect("utf-8 path"),
            "--asset-bundle",
            dir.path()
                .join("missing-bundle")
                .to_str()
                .expect("utf-8 path"),
        ])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("bundle not found"));
}

#[test]
fn watch_writes_initial_pdf_and_recompiles_on_change() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("hello.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\nHello\n\\end{document}\n",
    )
    .expect("write input file");

    let mut child = ferritex_bin()
        .args(["watch", tex_file.to_str().expect("utf-8 path")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn ferritex watch");

    let pdf_file = dir.path().join("hello.pdf");
    wait_until(
        || pdf_file.exists(),
        Duration::from_secs(2),
        "watch should emit the initial PDF",
    );
    let initial_modified = std::fs::metadata(&pdf_file)
        .expect("initial metadata")
        .modified()
        .expect("initial modified time");

    thread::sleep(Duration::from_millis(20));
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\nUpdated\n\\end{document}\n",
    )
    .expect("rewrite input file");

    wait_until(
        || {
            std::fs::metadata(&pdf_file)
                .and_then(|metadata| metadata.modified())
                .map(|modified| modified > initial_modified)
                .unwrap_or(false)
        },
        Duration::from_secs(2),
        "watch should recompile after a source change",
    );

    child.kill().expect("kill watch process");
    child.wait().expect("wait for watch process");
}

#[test]
fn watch_refreshes_dependency_set_after_new_input_is_added() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("main.tex");
    let appendix = dir.path().join("appendix.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\nInitial\n\\end{document}\n",
    )
    .expect("write input file");

    let mut child = ferritex_bin()
        .args(["watch", tex_file.to_str().expect("utf-8 path")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn ferritex watch");

    let pdf_file = dir.path().join("main.pdf");
    wait_until(
        || pdf_file.exists(),
        Duration::from_secs(2),
        "watch should emit the initial PDF",
    );
    let initial_modified = std::fs::metadata(&pdf_file)
        .expect("initial metadata")
        .modified()
        .expect("initial modified time");

    thread::sleep(Duration::from_millis(20));
    std::fs::write(&appendix, "Appendix v1\n").expect("write appendix");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\n\\input{appendix}\n\\end{document}\n",
    )
    .expect("rewrite main");

    wait_until(
        || {
            std::fs::metadata(&pdf_file)
                .and_then(|metadata| metadata.modified())
                .map(|modified| modified > initial_modified)
                .unwrap_or(false)
        },
        Duration::from_secs(2),
        "watch should recompile after adding a new input dependency",
    );
    let after_main_change = std::fs::metadata(&pdf_file)
        .expect("updated metadata")
        .modified()
        .expect("updated modified time");

    thread::sleep(Duration::from_millis(20));
    std::fs::write(&appendix, "Appendix v2\n").expect("rewrite appendix");

    wait_until(
        || {
            std::fs::metadata(&pdf_file)
                .and_then(|metadata| metadata.modified())
                .map(|modified| modified > after_main_change)
                .unwrap_or(false)
        },
        Duration::from_secs(2),
        "watch should pick up changes from a newly discovered dependency",
    );

    child.kill().expect("kill watch process");
    child.wait().expect("wait for watch process");
}

#[test]
fn lsp_initialize_and_diagnostics_work_over_stdio() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("main.tex");
    let uri = format!("file://{}", tex_file.to_str().expect("utf-8 path"));
    let mut child = ferritex_bin()
        .args(["lsp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn ferritex lsp");
    let mut stdin = child.stdin.take().expect("lsp stdin");
    let stdout = child.stdout.take().expect("lsp stdout");
    let mut reader = BufReader::new(stdout);

    write_lsp_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "processId": std::process::id(),
                "rootUri": format!("file://{}", dir.path().to_str().expect("utf-8 path")),
                "capabilities": {}
            }
        }),
    );
    let initialize = read_lsp_message(&mut reader);
    assert_eq!(initialize["id"], 1);
    assert_eq!(initialize["result"]["capabilities"]["textDocumentSync"], 1);
    assert!(initialize["result"]["capabilities"]["completionProvider"].is_object());
    assert_eq!(
        initialize["result"]["capabilities"]["codeActionProvider"],
        true
    );
    assert_eq!(
        initialize["result"]["capabilities"]["definitionProvider"],
        true
    );
    assert_eq!(initialize["result"]["capabilities"]["hoverProvider"], true);

    write_lsp_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {}
        }),
    );
    write_lsp_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": uri,
                    "languageId": "latex",
                    "version": 1,
                    "text": "\\documentclass{article}\n\\begin{document}\n\\begin{equation}\na=b\n\\end{document}\n"
                }
            }
        }),
    );

    let diagnostics = read_lsp_message(&mut reader);
    assert_eq!(diagnostics["method"], "textDocument/publishDiagnostics");
    let messages = diagnostics["params"]["diagnostics"]
        .as_array()
        .expect("diagnostics array")
        .iter()
        .filter_map(|diagnostic| diagnostic.get("message").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert!(messages
        .iter()
        .any(|message| message.contains("unclosed environment `equation`")));

    write_lsp_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "shutdown",
            "params": null
        }),
    );
    let shutdown = read_lsp_message(&mut reader);
    assert_eq!(shutdown["id"], 2);
    assert_eq!(shutdown["result"], Value::Null);
    write_lsp_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "exit",
            "params": null
        }),
    );

    assert!(child.wait().expect("wait lsp").success());
}

#[test]
fn lsp_definition_resolves_labels_from_included_files() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let chapter_dir = dir.path().join("chapters");
    std::fs::create_dir_all(&chapter_dir).expect("create chapter dir");
    std::fs::write(
        chapter_dir.join("figures.tex"),
        "\\label{fig:external}\nFigure content\n",
    )
    .expect("write included file");

    let tex_file = dir.path().join("main.tex");
    let uri = format!("file://{}", tex_file.to_str().expect("utf-8 path"));
    let mut child = ferritex_bin()
        .args(["lsp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn ferritex lsp");
    let mut stdin = child.stdin.take().expect("lsp stdin");
    let stdout = child.stdout.take().expect("lsp stdout");
    let mut reader = BufReader::new(stdout);

    write_lsp_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "processId": std::process::id(),
                "rootUri": format!("file://{}", dir.path().to_str().expect("utf-8 path")),
                "capabilities": {}
            }
        }),
    );
    let _initialize = read_lsp_message(&mut reader);

    write_lsp_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {}
        }),
    );
    write_lsp_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": uri,
                    "languageId": "latex",
                    "version": 1,
                    "text": "\\documentclass{article}\n\\begin{document}\n\\input{chapters/figures}\nSee \\ref{fig:external}.\n\\end{document}\n"
                }
            }
        }),
    );
    let _diagnostics = read_lsp_message(&mut reader);

    write_lsp_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "textDocument/definition",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": 3, "character": 13 }
            }
        }),
    );
    let definition = read_lsp_message(&mut reader);
    let expected_target = chapter_dir
        .join("figures.tex")
        .canonicalize()
        .expect("canonical included file");
    assert_eq!(definition["id"], 2);
    assert_eq!(
        definition["result"]["uri"],
        format!("file://{}", expected_target.to_str().expect("utf-8 path"))
    );
    assert_eq!(definition["result"]["range"]["start"]["line"], 0);

    write_lsp_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "shutdown",
            "params": null
        }),
    );
    let shutdown = read_lsp_message(&mut reader);
    assert_eq!(shutdown["id"], 3);
    assert_eq!(shutdown["result"], Value::Null);
    write_lsp_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "exit",
            "params": null
        }),
    );

    assert!(child.wait().expect("wait lsp").success());
}

#[test]
fn lsp_diagnostics_include_compile_errors() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("main.tex");
    let uri = format!("file://{}", tex_file.to_str().expect("utf-8 path"));
    let mut child = ferritex_bin()
        .args(["lsp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn ferritex lsp");
    let mut stdin = child.stdin.take().expect("lsp stdin");
    let stdout = child.stdout.take().expect("lsp stdout");
    let mut reader = BufReader::new(stdout);

    write_lsp_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "processId": std::process::id(),
                "rootUri": format!("file://{}", dir.path().to_str().expect("utf-8 path")),
                "capabilities": {}
            }
        }),
    );
    let _initialize = read_lsp_message(&mut reader);

    write_lsp_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {}
        }),
    );
    write_lsp_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": uri,
                    "languageId": "latex",
                    "version": 1,
                    "text": "\\documentclass{article}\n\\begin{document}\n\\begin{equation}\na=b\n"
                }
            }
        }),
    );

    let initial_diagnostics = read_lsp_message(&mut reader);
    let initial_messages = initial_diagnostics["params"]["diagnostics"]
        .as_array()
        .expect("diagnostics array")
        .iter()
        .filter_map(|diagnostic| diagnostic.get("message").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert!(initial_messages
        .iter()
        .any(|message| message.contains("missing \\end{document}")));
    assert!(initial_messages
        .iter()
        .any(|message| message.contains("unclosed environment `equation`")));

    write_lsp_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": {
                    "uri": uri,
                    "version": 2
                },
                "contentChanges": [
                    {
                        "text": "\\documentclass{article}\n\\begin{document}\n\\begin{equation}\na=b\n\\end{document}\n"
                    }
                ]
            }
        }),
    );

    let updated_diagnostics = read_lsp_message(&mut reader);
    let updated_messages = updated_diagnostics["params"]["diagnostics"]
        .as_array()
        .expect("diagnostics array")
        .iter()
        .filter_map(|diagnostic| diagnostic.get("message").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert!(!updated_messages
        .iter()
        .any(|message| message.contains("missing \\end{document}")));
    assert!(updated_messages
        .iter()
        .any(|message| message.contains("unclosed environment `equation`")));

    write_lsp_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "shutdown",
            "params": null
        }),
    );
    let shutdown = read_lsp_message(&mut reader);
    assert_eq!(shutdown["id"], 2);
    assert_eq!(shutdown["result"], Value::Null);
    write_lsp_message(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "exit",
            "params": null
        }),
    );

    assert!(child.wait().expect("wait lsp").success());
}

fn pdf_page_count(pdf: &str) -> usize {
    let marker = "/Count ";
    let start = pdf.find(marker).expect("pdf page count marker");
    let digits = pdf[start + marker.len()..]
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>();

    digits.parse().expect("parse pdf page count")
}

fn wait_until(mut condition: impl FnMut() -> bool, timeout: Duration, message: &str) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("{message}");
}

fn write_lsp_message(writer: &mut impl Write, value: &Value) {
    let body = serde_json::to_vec(value).expect("serialize LSP message");
    write!(writer, "Content-Length: {}\r\n\r\n", body.len()).expect("write header");
    writer.write_all(&body).expect("write body");
    writer.flush().expect("flush body");
}

fn read_lsp_message(reader: &mut impl BufRead) -> Value {
    let mut content_length = None;

    loop {
        let mut header = String::new();
        reader.read_line(&mut header).expect("read LSP header");
        if header == "\r\n" {
            break;
        }
        if let Some(value) = header.strip_prefix("Content-Length:") {
            content_length = Some(value.trim().parse::<usize>().expect("parse Content-Length"));
        }
    }

    let mut body = vec![0u8; content_length.expect("Content-Length header")];
    reader.read_exact(&mut body).expect("read LSP body");
    serde_json::from_slice(&body).expect("parse LSP body")
}
