use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use ferritex_bench::bench_fixtures_root;
use ferritex_core::kernel::api::SourceLocation;
use ferritex_core::synctex::SyncTexData;
use serde_json::{json, Value};

const PNG_1X1_RGB: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0,
    0, 0, 144, 119, 83, 222, 0, 0, 0, 12, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 0, 0, 3, 1,
    1, 0, 201, 254, 146, 239, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];
const MINIMAL_PDF: &[u8] = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources << /ProcSet [/PDF] >> /Contents 4 0 R >>\nendobj\n4 0 obj\n<< /Length 18 >>\nstream\n0 0 m\n200 100 l\nS\nendstream\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n";
const CORRUPT_PDF: &[u8] = b"%PDF-1.4\n1 0 obj\n<< /Type /Page /MediaBox [0 0 200] >>\n";

fn ferritex_bin() -> Command {
    let bin = env!("CARGO_BIN_EXE_ferritex");
    Command::new(bin)
}

fn read_synctex(path: &Path) -> SyncTexData {
    serde_json::from_slice(&std::fs::read(path).expect("read output synctex"))
        .expect("parse output synctex")
}

fn build_minimal_cmr10_tfm() -> Vec<u8> {
    const BC: u16 = 65;
    const EC: u16 = 66;
    const LH: u16 = 2;
    const NW: u16 = 2;
    const NH: u16 = 2;
    const ND: u16 = 1;
    const NI: u16 = 1;
    const CHECKSUM: u32 = 0xABCD_1234;
    const DESIGN_SIZE_FIXWORD: i32 = 10_485_760;

    let char_count = usize::from(EC - BC + 1);
    let lf = 6
        + usize::from(LH)
        + char_count
        + usize::from(NW)
        + usize::from(NH)
        + usize::from(ND)
        + usize::from(NI);

    let mut bytes = Vec::with_capacity(lf * 4);
    for value in [lf as u16, LH, BC, EC, NW, NH, ND, NI, 0, 0, 0, 0] {
        bytes.extend_from_slice(&value.to_be_bytes());
    }

    bytes.extend_from_slice(&CHECKSUM.to_be_bytes());
    bytes.extend_from_slice(&DESIGN_SIZE_FIXWORD.to_be_bytes());

    for _ in 0..char_count {
        bytes.extend_from_slice(&[1, 0x10, 0, 0]);
    }

    for value in [0_i32, 349_525] {
        bytes.extend_from_slice(&value.to_be_bytes());
    }
    for value in [0_i32, 104_858] {
        bytes.extend_from_slice(&value.to_be_bytes());
    }
    bytes.extend_from_slice(&0_i32.to_be_bytes());
    bytes.extend_from_slice(&0_i32.to_be_bytes());

    bytes
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
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.trim().is_empty());
    assert!(stdout.contains(tex_file.to_str().expect("utf-8 path")));
    assert!(stdout.contains("hello.pdf"));
    assert!(stdout.contains("(1 page)"));

    let pdf_file = dir.path().join("hello.pdf");
    let pdf = std::fs::read_to_string(&pdf_file).expect("read output pdf");
    assert!(pdf.starts_with("%PDF-1.4"));
    assert!(pdf.contains("Hello, Ferritex!"));
    assert!(!pdf.contains("Ferritex placeholder PDF"));
}

#[test]
fn compile_with_warnings_prints_summary_including_warning_count() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("warn.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\nHello δ\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(
        output.status.code(),
        Some(1),
        "warnings should exit with code 1"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("->"),
        "summary should contain arrow separator"
    );
    assert!(stdout.contains(".pdf"), "summary should mention PDF output");
    assert!(
        stdout.contains("warning"),
        "summary should mention warnings"
    );
}

#[test]
fn compile_with_trace_font_tasks_emits_font_task_trace_to_stderr() {
    // Cold path: the first compile should emit concrete font task traces.
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
fn compile_with_trace_font_tasks_emits_trace_on_warm_cache_hit() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("hello.tex");
    let output_dir = dir.path().join("build");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\nHello warm cache\n\\end{document}\n",
    )
    .expect("write input file");

    let first = ferritex_bin()
        .args([
            "compile",
            "--output-dir",
            output_dir.to_str().expect("utf-8 output dir"),
            "--trace-font-tasks",
            tex_file.to_str().expect("utf-8 path"),
        ])
        .output()
        .expect("failed to run ferritex");
    assert_eq!(
        first.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let cold_stderr = String::from_utf8_lossy(&first.stderr);
    assert!(cold_stderr.contains("\"fontTaskId\""));
    assert!(!cold_stderr.contains("error:"));

    let cache_dir = output_dir.join(".ferritex-cache");
    let cache_entries = std::fs::read_dir(&cache_dir).expect("read warm cache dir");
    assert!(
        cache_entries.count() > 0,
        "warm cache dir should contain at least one cache record"
    );

    let second = ferritex_bin()
        .args([
            "compile",
            "--output-dir",
            output_dir.to_str().expect("utf-8 output dir"),
            "--trace-font-tasks",
            tex_file.to_str().expect("utf-8 path"),
        ])
        .output()
        .expect("failed to run ferritex");
    assert_eq!(
        second.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    let warm_stderr = String::from_utf8_lossy(&second.stderr);
    assert_eq!(
        warm_stderr.lines().count(),
        1,
        "warm cache hit must emit exactly one FontTaskTrace sentinel line, got: {warm_stderr}"
    );
    assert!(warm_stderr.contains("\"fontTaskId\""));
    assert!(warm_stderr.contains("\"fontAsset\""));
    assert!(warm_stderr.contains("\"startedAt\""));
    assert!(warm_stderr.contains("\"finishedAt\""));
    assert!(warm_stderr.contains("\"workerId\""));
    assert!(warm_stderr.contains("font-load-cache-hit"));
    assert!(warm_stderr.contains("builtin:font-cache-hit"));
    assert!(
        !warm_stderr.contains("font-load-main"),
        "warm cache hit must not re-run cold-path main font load: {warm_stderr}"
    );
    assert!(
        !warm_stderr.contains("font-load-basic-fallback"),
        "warm cache hit must not re-run cold-path basic fallback: {warm_stderr}"
    );
    assert!(!warm_stderr.contains("error:"));
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
fn compile_includegraphics_embeds_pdf_as_form_xobject() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("vector.tex");
    let source_pdf = dir.path().join("diagram.pdf");
    std::fs::write(&source_pdf, MINIMAL_PDF).expect("write source pdf");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\nBefore\n\\includegraphics[width=100pt]{diagram.pdf}\nAfter\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));

    let pdf = std::fs::read(dir.path().join("vector.pdf")).expect("read output pdf");
    let content = String::from_utf8_lossy(&pdf);
    assert!(content.contains("/Subtype /Form"));
    assert!(content.contains("/BBox [0 0 200 100]"));
    assert!(content.contains("/Resources << /ProcSet [/PDF] >>"));
    assert!(content.contains("/XObject << /Fm1"));
    assert!(content.contains("/Fm1 Do"));
    assert!(content.contains("0 0 m\n200 100 l\nS"));
    assert_eq!(content.matches("%PDF-1.4").count(), 1);
}

#[test]
fn compile_includegraphics_reports_invalid_pdf_input() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("broken.tex");
    let source_pdf = dir.path().join("broken-asset.pdf");
    std::fs::write(&source_pdf, CORRUPT_PDF).expect("write corrupt pdf");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\n\\includegraphics{broken-asset.pdf}\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("invalid PDF input for \\includegraphics"));
    assert!(stderr
        .contains("help: use an unencrypted single-page PDF whose first page defines /MediaBox"));
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
fn compile_with_synctex_writes_searchable_sidecar_with_float_caption_fragment() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("synctex.tex");
    let image_file = dir.path().join("pixel.png");
    std::fs::write(&image_file, PNG_1X1_RGB).expect("write image file");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\nPrelude\n\\begin{figure}[h]\n\\includegraphics[width=100pt]{pixel.png}\n\\caption{Embedded pixel}\n\\label{fig:pixel}\n\\end{figure}\n\\newpage\nSecond page text\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args([
            "compile",
            "--synctex",
            tex_file.to_str().expect("utf-8 path"),
        ])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));

    let synctex_path = dir.path().join("synctex.synctex");
    assert!(synctex_path.exists());
    let synctex = read_synctex(&synctex_path);

    let caption_positions = synctex.forward_search(SourceLocation {
        file_id: 0,
        line: 6,
        column: 10,
    });
    assert_eq!(caption_positions.len(), 1);
    assert_eq!(
        synctex
            .inverse_search(caption_positions[0])
            .map(|span| span.start.line),
        Some(6)
    );
    assert!(synctex
        .fragments
        .iter()
        .any(|fragment| fragment.text == "Figure 1: Embedded pixel"));
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
fn compile_renders_flalign_environment() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("flalign.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\usepackage{amsmath}\n\\begin{document}\n\\begin{flalign}\na &= b && c &= d \\\\\ne &= f && g &= h\n\\end{flalign}\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));
    let pdf = std::fs::read_to_string(dir.path().join("flalign.pdf")).expect("read output pdf");
    assert!(pdf.contains("a=b"));
    assert!(pdf.contains("c=d"));
    assert!(pdf.contains("e=f"));
    assert!(pdf.contains("g=h"));
}

#[test]
fn compile_renders_alignat_environment() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("alignat.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\usepackage{amsmath}\n\\begin{document}\n\\begin{alignat}{2}\na &= b & c &= d \\\\\ne &= f & g &= h\n\\end{alignat}\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));
    let pdf = std::fs::read_to_string(dir.path().join("alignat.pdf")).expect("read output pdf");
    assert!(pdf.contains("a=b"));
    assert!(pdf.contains("c=d"));
    assert!(pdf.contains("e=f"));
    assert!(pdf.contains("g=h"));
}

#[test]
fn compile_renders_split_inside_equation() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("split.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\usepackage{amsmath}\n\\begin{document}\n\\begin{equation}\n\\begin{split}\na &= b + c \\\\\nd &= e + f\n\\end{split}\n\\end{equation}\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));
    let pdf = std::fs::read_to_string(dir.path().join("split.pdf")).expect("read output pdf");
    assert!(pdf.contains("a=b+c"));
    assert!(pdf.contains("d=e+f"));
}

#[test]
fn compile_renders_intertext_in_align() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("intertext.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\usepackage{amsmath}\n\\begin{document}\n\\begin{align}\na &= b \\\\\n\\intertext{text here}\nc &= d\n\\end{align}\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));
    let pdf = std::fs::read_to_string(dir.path().join("intertext.pdf")).expect("read output pdf");
    assert!(pdf.contains("a=b"));
    assert!(pdf.contains("text here"));
    assert!(pdf.contains("c=d"));
}

#[test]
fn compile_renders_substack_in_sum() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("substack.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\usepackage{amsmath}\n\\begin{document}\n\\[\n\\sum_{\\substack{i=1 \\\\ j=2}} a_{ij}\n\\]\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));
    let pdf = std::fs::read_to_string(dir.path().join("substack.pdf")).expect("read output pdf");
    assert!(pdf.contains("i=1, j=2"));
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

fn expanded_asset_bundle_fixture() -> tempfile::TempDir {
    let bundle_dir = tempfile::tempdir().expect("create asset bundle tempdir");
    let bundle_root = bundle_dir.path();
    let texmf_root = bundle_root.join("texmf");
    let package_sty = texmf_root.join("tex/latex/ferritex/bundlebootstrap.sty");
    let bundled_tex = texmf_root.join("bundled.tex");
    let custom_tex = texmf_root.join("shared/custom.tex");
    let cmr10_tfm = texmf_root.join("fonts/tfm/public/cm/cmr10.tfm");

    std::fs::create_dir_all(package_sty.parent().expect("bundlebootstrap.sty parent"))
        .expect("create package directory");
    std::fs::create_dir_all(custom_tex.parent().expect("custom tex parent"))
        .expect("create custom tex directory");
    std::fs::create_dir_all(cmr10_tfm.parent().expect("cmr10.tfm parent"))
        .expect("create TFM font directory");

    std::fs::write(
        bundle_root.join("manifest.json"),
        serde_json::to_vec(&json!({
            "name": "expanded-basic",
            "version": "2026.03.29",
            "min_ferritex_version": "0.1.0",
            "format_version": 1,
            "asset_index_path": "asset-index.json",
        }))
        .expect("serialize bundle manifest"),
    )
    .expect("write bundle manifest");
    std::fs::write(
        bundle_root.join("asset-index.json"),
        serde_json::to_vec(&json!({
            "tex_inputs": {
                "bundled": "texmf/bundled.tex",
                "bundled.tex": "texmf/bundled.tex",
                "shared/custom": "texmf/shared/custom.tex",
                "shared/custom.tex": "texmf/shared/custom.tex",
            },
            "packages": {
                "bundlebootstrap": "texmf/tex/latex/ferritex/bundlebootstrap.sty",
                "bundlebootstrap.sty": "texmf/tex/latex/ferritex/bundlebootstrap.sty",
            },
            "opentype_fonts": {},
            "tfm_fonts": {
                "cmr10": "texmf/fonts/tfm/public/cm/cmr10.tfm",
            },
            "default_opentype_fonts": [],
        }))
        .expect("serialize bundle asset index"),
    )
    .expect("write bundle asset index");
    std::fs::write(
        &package_sty,
        "% Minimal bundle package stub for asset index coverage.\n",
    )
    .expect("write bundlebootstrap.sty");
    std::fs::write(&bundled_tex, "Bundled from expanded asset bundle.\n")
        .expect("write bundled tex input");
    std::fs::write(&custom_tex, "Bundle version of custom.\n").expect("write custom tex input");
    std::fs::write(&cmr10_tfm, build_minimal_cmr10_tfm()).expect("write cmr10 TFM");

    bundle_dir
}

#[test]
fn compile_resolves_tex_input_from_asset_bundle_outside_project_root() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let project_root = dir.path().join("project");
    let bundle_root = dir.path().join("bundle");
    let cmr10_tfm = bundle_root.join("texmf/fonts/tfm/public/cm/cmr10.tfm");
    std::fs::create_dir_all(project_root.join("src")).expect("create source tree");
    std::fs::create_dir_all(bundle_root.join("texmf")).expect("create bundle texmf");
    std::fs::create_dir_all(cmr10_tfm.parent().expect("cmr10 parent")).expect("create tfm dir");
    std::fs::write(
        bundle_root.join("manifest.json"),
        r#"{"name":"default","version":"2026.03.18","min_ferritex_version":"0.1.0","format_version":1,"asset_index_path":"asset-index.json"}"#,
    )
    .expect("write bundle manifest");
    std::fs::write(
        bundle_root.join("asset-index.json"),
        r#"{"tex_inputs":{"bundled.tex":"texmf/bundled.tex"},"packages":{},"opentype_fonts":{},"tfm_fonts":{"cmr10":"texmf/fonts/tfm/public/cm/cmr10.tfm"},"default_opentype_fonts":[]}"#,
    )
    .expect("write bundle asset index");
    std::fs::write(
        bundle_root.join("texmf/bundled.tex"),
        "Bundled from asset bundle.\n",
    )
    .expect("write bundled tex input");
    std::fs::write(&cmr10_tfm, build_minimal_cmr10_tfm()).expect("write cmr10 TFM");
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
fn compile_bundle_only_bootstrap_succeeds() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let bundle_dir = expanded_asset_bundle_fixture();
    let tex_file = dir.path().join("main.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\n\\input{bundled}\nBundle bootstrap test.\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args([
            "compile",
            tex_file.to_str().expect("utf-8 path"),
            "--asset-bundle",
            bundle_dir.path().to_str().expect("utf-8 bundle path"),
        ])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.trim().is_empty());

    let pdf = std::fs::read_to_string(dir.path().join("main.pdf")).expect("read output pdf");
    assert!(pdf.contains("Bundled from expanded asset bundle."));
    assert!(pdf.contains("Bundle bootstrap test."));
}

#[test]
fn compile_bundle_tfm_font_resolution() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let bundle_dir = expanded_asset_bundle_fixture();
    let tex_file = dir.path().join("font-test.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\nFont test.\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args([
            "compile",
            tex_file.to_str().expect("utf-8 path"),
            "--asset-bundle",
            bundle_dir.path().to_str().expect("utf-8 bundle path"),
        ])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));

    let pdf = std::fs::read_to_string(dir.path().join("font-test.pdf")).expect("read output pdf");
    assert!(pdf.contains("Font test."));
}

#[test]
fn compile_bundle_only_fails_when_required_tfm_removed() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let bundle_dir = expanded_asset_bundle_fixture();
    let tfm_path = bundle_dir
        .path()
        .join("texmf/fonts/tfm/public/cm/cmr10.tfm");
    let asset_index_path = bundle_dir.path().join("asset-index.json");
    let tex_file = dir.path().join("article.tex");
    let pdf_file = dir.path().join("article.pdf");

    std::fs::remove_file(&tfm_path).expect("remove cmr10.tfm from bundle");

    let mut asset_index: Value =
        serde_json::from_slice(&std::fs::read(&asset_index_path).expect("read bundle asset index"))
            .expect("parse bundle asset index");
    asset_index["tfm_fonts"]
        .as_object_mut()
        .expect("tfm_fonts should be an object")
        .remove("cmr10");
    std::fs::write(
        &asset_index_path,
        serde_json::to_vec(&asset_index).expect("serialize modified asset index"),
    )
    .expect("write modified asset index");

    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\nBundle-only font failure.\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args([
            "compile",
            tex_file.to_str().expect("utf-8 path"),
            "--asset-bundle",
            bundle_dir.path().to_str().expect("utf-8 bundle path"),
        ])
        .output()
        .expect("failed to run ferritex");

    assert_ne!(output.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("required asset bundle font metrics \"cmr10\" could not be resolved"));
    assert!(
        !pdf_file.exists(),
        "bundle-only failure must not emit a PDF"
    );
}

#[test]
fn compile_reproducible_with_asset_bundle() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let bundle_dir = expanded_asset_bundle_fixture();
    let tex_file = dir.path().join("main.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\nReproducible bundle output.\n\\end{document}\n",
    )
    .expect("write input file");

    let run_compile = || {
        ferritex_bin()
            .args([
                "compile",
                "--reproducible",
                tex_file.to_str().expect("utf-8 path"),
                "--asset-bundle",
                bundle_dir.path().to_str().expect("utf-8 bundle path"),
            ])
            .output()
            .expect("failed to run ferritex")
    };

    let first = run_compile();
    assert_eq!(first.status.code(), Some(0));
    let first_pdf = std::fs::read(dir.path().join("main.pdf")).expect("read first output pdf");

    let second = run_compile();
    assert_eq!(second.status.code(), Some(0));
    let second_pdf = std::fs::read(dir.path().join("main.pdf")).expect("read second output pdf");

    assert_eq!(first_pdf, second_pdf);
}

#[test]
fn incremental_recompile_matches_full_compile() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let incremental_out = dir.path().join("incremental-out");
    let full_out = dir.path().join("full-out");
    std::fs::write(
        dir.path().join("main.tex"),
        "\\documentclass{report}\n\\begin{document}\n\\input{chapter-one}\n\\newpage\n\\input{chapter-two}\n\\end{document}\n",
    )
    .expect("write main");
    std::fs::write(
        dir.path().join("chapter-one.tex"),
        "\\chapter{One}\nStable chapter body.\n",
    )
    .expect("write chapter one");
    std::fs::write(
        dir.path().join("chapter-two.tex"),
        "\\chapter{Two}\nOriginal chapter two body.\n",
    )
    .expect("write chapter two");

    let first = ferritex_bin()
        .current_dir(dir.path())
        .args([
            "compile",
            "main.tex",
            "--output-dir",
            incremental_out.to_str().expect("utf-8 output dir"),
        ])
        .output()
        .expect("failed to run ferritex");
    assert_eq!(
        first.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );

    std::fs::write(
        dir.path().join("chapter-two.tex"),
        "\\chapter{Two}\nUpdated chapter two body for incremental compile.\n",
    )
    .expect("update chapter two");

    let second = ferritex_bin()
        .current_dir(dir.path())
        .args([
            "compile",
            "main.tex",
            "--output-dir",
            incremental_out.to_str().expect("utf-8 output dir"),
        ])
        .output()
        .expect("failed to run ferritex");
    assert_eq!(
        second.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );

    let full = ferritex_bin()
        .current_dir(dir.path())
        .args([
            "compile",
            "main.tex",
            "--output-dir",
            full_out.to_str().expect("utf-8 output dir"),
            "--no-cache",
        ])
        .output()
        .expect("failed to run ferritex");
    assert_eq!(
        full.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&full.stderr)
    );

    let incremental_pdf =
        std::fs::read(incremental_out.join("main.pdf")).expect("read incremental pdf");
    let full_pdf = std::fs::read(full_out.join("main.pdf")).expect("read full pdf");
    assert_eq!(incremental_pdf, full_pdf);
}

#[test]
fn incremental_xref_convergence_after_page_shift() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let incremental_out = dir.path().join("incremental-out");
    let full_out = dir.path().join("full-out");
    std::fs::write(
        dir.path().join("main.tex"),
        concat!(
            "\\documentclass{report}\n",
            "\\begin{document}\n",
            "\\tableofcontents\n",
            "\\input{chapter-one}\n",
            "\\input{chapter-two}\n",
            "\\input{chapter-three}\n",
            "\\end{document}\n",
        ),
    )
    .expect("write main");
    std::fs::write(
        dir.path().join("chapter-one.tex"),
        concat!(
            "\\chapter{One}\\label{chap:one}\n",
            "Chapter one opens the report and points ahead to Chapter \\ref{chap:three} on page \\pageref{chap:three}.\n",
            "The initial body stays short so later chapters begin earlier.\n",
        ),
    )
    .expect("write chapter one");
    std::fs::write(
        dir.path().join("chapter-two.tex"),
        concat!(
            "\\chapter{Two}\\label{chap:two}\n",
            "Chapter two points back to Chapter \\ref{chap:one} on page \\pageref{chap:one}.\n",
            "Chapter two also points ahead to Chapter \\ref{chap:three} on page \\pageref{chap:three}.\n",
        ),
    )
    .expect("write chapter two");
    std::fs::write(
        dir.path().join("chapter-three.tex"),
        concat!(
            "\\chapter{Three}\\label{chap:three}\n",
            "Chapter three points back to Chapter \\ref{chap:one} on page \\pageref{chap:one}.\n",
            "Chapter three also points back to Chapter \\ref{chap:two} on page \\pageref{chap:two}.\n",
        ),
    )
    .expect("write chapter three");

    let first = ferritex_bin()
        .current_dir(dir.path())
        .args([
            "compile",
            "main.tex",
            "--output-dir",
            incremental_out.to_str().expect("utf-8 output dir"),
        ])
        .output()
        .expect("failed to run ferritex");
    assert_eq!(
        first.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );

    std::fs::write(
        dir.path().join("chapter-one.tex"),
        concat!(
            "\\chapter{One}\\label{chap:one}\n",
            "Chapter one opens the report and points ahead to Chapter \\ref{chap:three} on page \\pageref{chap:three}.\n",
            "The updated body now forces later chapters onto different pages.\n",
            "\\newpage\n",
            "Expanded material keeps chapter one running longer before chapter two starts.\n",
            "\\newpage\n",
            "A final block of expanded material ensures the shifted layout persists.\n",
        ),
    )
    .expect("update chapter one");

    let second = ferritex_bin()
        .current_dir(dir.path())
        .args([
            "compile",
            "main.tex",
            "--output-dir",
            incremental_out.to_str().expect("utf-8 output dir"),
        ])
        .output()
        .expect("failed to run ferritex");
    assert_eq!(
        second.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );

    let full = ferritex_bin()
        .current_dir(dir.path())
        .args([
            "compile",
            "main.tex",
            "--output-dir",
            full_out.to_str().expect("utf-8 output dir"),
            "--no-cache",
        ])
        .output()
        .expect("failed to run ferritex");
    assert_eq!(
        full.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&full.stderr)
    );

    let incremental_pdf =
        std::fs::read(incremental_out.join("main.pdf")).expect("read incremental pdf");
    let full_pdf = std::fs::read(full_out.join("main.pdf")).expect("read full pdf");
    assert_eq!(incremental_pdf, full_pdf);
}

#[test]
fn compile_multi_chapter_with_jobs_4_matches_jobs_1() {
    let write_project = |root: &std::path::Path| {
        std::fs::write(
            root.join("main.tex"),
            "\\documentclass{report}\n\\begin{document}\n\\input{chapter-one}\n\\newpage\n\\input{chapter-two}\n\\newpage\n\\input{chapter-three}\n\\end{document}\n",
        )
        .expect("write main");
        std::fs::write(
            root.join("chapter-one.tex"),
            "\\chapter{One}\\label{chap:one}\nOriginal chapter one body.\n",
        )
        .expect("write chapter one");
        std::fs::write(
            root.join("chapter-two.tex"),
            "\\chapter{Two}\\label{chap:two}\nOriginal chapter two body.\n",
        )
        .expect("write chapter two");
        std::fs::write(
            root.join("chapter-three.tex"),
            "\\chapter{Three}\\label{chap:three}\nStable chapter three body.\n",
        )
        .expect("write chapter three");
    };
    let update_project = |root: &std::path::Path| {
        std::fs::write(
            root.join("chapter-one.tex"),
            "\\chapter{One}\\label{chap:one}\nEdited chapter one body.\n",
        )
        .expect("update chapter one");
        std::fs::write(
            root.join("chapter-two.tex"),
            "\\chapter{Two}\\label{chap:two}\nEdited chapter two body.\n",
        )
        .expect("update chapter two");
    };
    let run_incremental = |root: &std::path::Path, output_dir: &std::path::Path, jobs: &str| {
        let warmup = ferritex_bin()
            .current_dir(root)
            .args([
                "compile",
                "main.tex",
                "--output-dir",
                output_dir.to_str().expect("utf-8 output dir"),
                "--jobs",
                jobs,
            ])
            .output()
            .expect("failed to run warmup compile");
        assert_eq!(
            warmup.status.code(),
            Some(0),
            "stderr: {}",
            String::from_utf8_lossy(&warmup.stderr)
        );

        update_project(root);

        let incremental = ferritex_bin()
            .current_dir(root)
            .args([
                "compile",
                "main.tex",
                "--output-dir",
                output_dir.to_str().expect("utf-8 output dir"),
                "--jobs",
                jobs,
            ])
            .output()
            .expect("failed to run incremental compile");
        assert_eq!(
            incremental.status.code(),
            Some(0),
            "stderr: {}",
            String::from_utf8_lossy(&incremental.stderr)
        );

        std::fs::read(output_dir.join("main.pdf")).expect("read output pdf")
    };

    let sequential_dir = tempfile::tempdir().expect("create sequential tempdir");
    write_project(sequential_dir.path());
    let sequential_pdf = run_incremental(
        sequential_dir.path(),
        &sequential_dir.path().join("out"),
        "1",
    );

    let parallel_dir = tempfile::tempdir().expect("create parallel tempdir");
    write_project(parallel_dir.path());
    let parallel_pdf = run_incremental(parallel_dir.path(), &parallel_dir.path().join("out"), "4");

    assert_eq!(sequential_pdf, parallel_pdf);
}

#[test]
fn corrupted_cache_triggers_safe_fallback_in_incremental_mode() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let output_dir = dir.path().join("out");
    std::fs::write(
        dir.path().join("main.tex"),
        "\\documentclass{article}\n\\begin{document}\nInitial cacheable body.\n\\end{document}\n",
    )
    .expect("write main");

    let first = ferritex_bin()
        .current_dir(dir.path())
        .args([
            "compile",
            "main.tex",
            "--output-dir",
            output_dir.to_str().expect("utf-8 output dir"),
        ])
        .output()
        .expect("failed to run ferritex");
    assert_eq!(
        first.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );

    let record_dir = std::fs::read_dir(output_dir.join(".ferritex-cache"))
        .expect("read cache dir")
        .map(|entry| entry.expect("cache entry").path())
        .find(|path| path.is_dir())
        .expect("cache record directory");
    let index_bin = record_dir.join("index.bin");
    assert!(
        index_bin.exists(),
        "index.bin should exist in cache record directory"
    );
    std::fs::write(&index_bin, b"corrupted binary data").expect("corrupt cache index");

    std::fs::write(
        dir.path().join("main.tex"),
        "\\documentclass{article}\n\\begin{document}\nUpdated body after cache corruption.\n\\end{document}\n",
    )
    .expect("update main");

    let second = ferritex_bin()
        .current_dir(dir.path())
        .args([
            "compile",
            "main.tex",
            "--output-dir",
            output_dir.to_str().expect("utf-8 output dir"),
        ])
        .output()
        .expect("failed to run ferritex");
    assert_eq!(
        second.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );

    let pdf = std::fs::read_to_string(output_dir.join("main.pdf")).expect("read output pdf");
    assert!(pdf.contains("Updated body after cache corruption."));
}

#[test]
fn compile_bundle_fallback_resolves_tex_input_not_in_project() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let bundle_dir = expanded_asset_bundle_fixture();
    let project_root = dir.path().join("project");
    let src_dir = project_root.join("src");
    std::fs::create_dir_all(&src_dir).expect("create source tree");
    std::fs::write(
        src_dir.join("main.tex"),
        "\\documentclass{article}\n\\begin{document}\n\\input{shared/custom}\n\\end{document}\n",
    )
    .expect("write main");
    std::fs::write(
        project_root.join("custom.tex"),
        "Host version of custom should not resolve.\n",
    )
    .expect("write unmatched host tex input");

    // tex_input resolution prefers project-local paths before the asset bundle, so this
    // only verifies bundle fallback for a lookup key that is absent from the project tree.
    let output = ferritex_bin()
        .current_dir(&project_root)
        .args([
            "compile",
            "src/main.tex",
            "--asset-bundle",
            bundle_dir.path().to_str().expect("utf-8 bundle path"),
        ])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.trim().is_empty());

    let pdf = std::fs::read_to_string(project_root.join("src/main.pdf")).expect("read output pdf");
    assert!(pdf.contains("Bundle version of custom."));
    assert!(!pdf.contains("Host version of custom should not resolve."));
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
fn compile_with_corrupted_manifest_reports_diagnostic() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let bundle_dir = expanded_asset_bundle_fixture();
    let tex_file = dir.path().join("main.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\nBroken manifest.\n\\end{document}\n",
    )
    .expect("write input file");
    std::fs::write(bundle_dir.path().join("manifest.json"), "{ not valid json")
        .expect("corrupt bundle manifest");

    let output = ferritex_bin()
        .args([
            "compile",
            tex_file.to_str().expect("utf-8 path"),
            "--asset-bundle",
            bundle_dir.path().to_str().expect("utf-8 bundle path"),
        ])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("invalid manifest"));
    assert!(stderr.contains("help: verify the asset bundle path and version"));
}

#[test]
fn compile_with_incompatible_bundle_version_reports_diagnostic() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let bundle_dir = expanded_asset_bundle_fixture();
    let tex_file = dir.path().join("main.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\nIncompatible bundle.\n\\end{document}\n",
    )
    .expect("write input file");
    std::fs::write(
        bundle_dir.path().join("manifest.json"),
        serde_json::to_vec(&json!({
            "name": "expanded-basic",
            "version": "2026.03.29",
            "min_ferritex_version": "99.0.0",
            "format_version": 1,
            "asset_index_path": "asset-index.json",
        }))
        .expect("serialize incompatible manifest"),
    )
    .expect("write incompatible bundle manifest");

    let output = ferritex_bin()
        .args([
            "compile",
            tex_file.to_str().expect("utf-8 path"),
            "--asset-bundle",
            bundle_dir.path().to_str().expect("utf-8 bundle path"),
        ])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("version incompatible"));
    assert!(stderr.contains("required 99.0.0"));
    assert!(stderr.contains("help: verify the asset bundle path and version"));
}

#[test]
fn compile_with_missing_asset_index_reports_diagnostic() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let bundle_dir = expanded_asset_bundle_fixture();
    let tex_file = dir.path().join("main.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\nMissing index.\n\\end{document}\n",
    )
    .expect("write input file");
    std::fs::remove_file(bundle_dir.path().join("asset-index.json"))
        .expect("remove bundle asset index");

    let output = ferritex_bin()
        .args([
            "compile",
            tex_file.to_str().expect("utf-8 path"),
            "--asset-bundle",
            bundle_dir.path().to_str().expect("utf-8 bundle path"),
        ])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("asset index not found"));
    assert!(stderr.contains("help: verify the asset bundle path and version"));
}

#[test]
fn compile_with_unsupported_format_version_reports_diagnostic() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let bundle_dir = expanded_asset_bundle_fixture();
    let tex_file = dir.path().join("main.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\nUnsupported format.\n\\end{document}\n",
    )
    .expect("write input file");
    std::fs::write(
        bundle_dir.path().join("manifest.json"),
        serde_json::to_vec(&json!({
            "name": "expanded-basic",
            "version": "2026.03.29",
            "min_ferritex_version": "0.1.0",
            "format_version": 99,
            "asset_index_path": "asset-index.json",
        }))
        .expect("serialize unsupported manifest"),
    )
    .expect("write unsupported bundle manifest");

    let output = ferritex_bin()
        .args([
            "compile",
            tex_file.to_str().expect("utf-8 path"),
            "--asset-bundle",
            bundle_dir.path().to_str().expect("utf-8 bundle path"),
        ])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unsupported bundle format version"));
    assert!(stderr.contains("help: verify the asset bundle path and version"));
}

#[test]
fn compile_with_corrupted_bundle_format_produces_no_pdf() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let bundle_dir = expanded_asset_bundle_fixture();
    let tex_file = dir.path().join("main.tex");
    let pdf_file = dir.path().join("main.pdf");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\nCorrupted format.\n\\end{document}\n",
    )
    .expect("write input file");
    std::fs::write(
        bundle_dir.path().join("manifest.json"),
        serde_json::to_vec(&json!({
            "name": "expanded-basic",
            "version": "2026.03.29",
            "min_ferritex_version": "0.1.0",
            "format_version": 99,
            "asset_index_path": "asset-index.json",
        }))
        .expect("serialize unsupported manifest"),
    )
    .expect("write corrupted bundle manifest");

    let output = ferritex_bin()
        .args([
            "compile",
            tex_file.to_str().expect("utf-8 path"),
            "--asset-bundle",
            bundle_dir.path().to_str().expect("utf-8 bundle path"),
        ])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unsupported bundle format version"));
    assert!(stderr.contains("help: verify the asset bundle path and version"));
    assert!(!pdf_file.exists(), "corrupted bundle must not emit a PDF");
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
fn compile_unsupported_document_class_reports_supported_set() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("revtex.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{revtex4-1}\n\\begin{document}\nHello\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unsupported document class"));
    assert!(stderr.contains("revtex4-1"));
    assert!(stderr.contains("article"));
    assert!(stderr.contains("report"));
    assert!(stderr.contains("book"));
    assert!(stderr.contains("letter"));
}

#[test]
fn compile_malformed_documentclass_reports_invalid_declaration() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("malformed.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{\n}\n\\begin{document}\nHello\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("invalid \\documentclass declaration"));
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
fn compile_reports_issue_1_diagnostics_and_exits_nonzero() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("broken.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\nHello\n\\nonexistentcommand{foo}\n\\begin{unclosedenv}\ntext\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("undefined control sequence `\\nonexistentcommand`"));
    assert!(stderr.contains("unclosed environment `unclosedenv`"));
    assert!(dir.path().join("broken.pdf").exists());
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
    assert!(stderr.contains("help: verify the asset bundle path and version"));
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
fn watch_exits_with_friendly_message_when_watched_directory_is_deleted() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let tex_file = workspace.join("hello.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\nHello\n\\end{document}\n",
    )
    .expect("write input file");

    let mut child = ferritex_bin()
        .args(["watch", tex_file.to_str().expect("utf-8 path")])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ferritex watch");

    let pdf_file = workspace.join("hello.pdf");
    wait_until(
        || pdf_file.exists(),
        Duration::from_secs(2),
        "watch should emit the initial PDF",
    );

    std::fs::remove_dir_all(&workspace).expect("delete workspace directory");

    let exit_status = wait_for_exit(&mut child, Duration::from_secs(5))
        .expect("watch should exit after the workspace is removed");
    assert_eq!(
        exit_status.code(),
        Some(2),
        "watch should exit with status 2 after losing its workspace",
    );

    let mut stderr_buf = String::new();
    child
        .stderr
        .take()
        .expect("captured stderr")
        .read_to_string(&mut stderr_buf)
        .expect("read stderr");

    assert!(
        stderr_buf.contains("no longer exists"),
        "stderr should explain that the watched path is gone: {stderr_buf}",
    );
    assert!(
        stderr_buf.contains("help:"),
        "stderr should include a help line guiding the user: {stderr_buf}",
    );
    assert!(
        stderr_buf.contains("rerun `ferritex watch`"),
        "stderr should suggest rerunning watch after restoring the path: {stderr_buf}",
    );
    assert!(
        !stderr_buf.contains("os error"),
        "stderr should not surface the raw OS error: {stderr_buf}",
    );
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

#[test]
fn lsp_diagnostics_include_source_and_context() {
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

    let diagnostics = read_lsp_message(&mut reader);
    assert_eq!(diagnostics["method"], "textDocument/publishDiagnostics");
    let diagnostics = diagnostics["params"]["diagnostics"]
        .as_array()
        .expect("diagnostics array");
    assert!(!diagnostics.is_empty());
    assert!(diagnostics.iter().all(|diagnostic| {
        diagnostic["source"] == "ferritex"
            && diagnostic.get("range").is_some()
            && diagnostic["range"]["start"]["line"].is_u64()
            && diagnostic["message"]
                .as_str()
                .is_some_and(|message| !message.is_empty())
            && diagnostic["data"]["context"]
                .as_str()
                .is_some_and(|context| !context.is_empty())
            && diagnostic["data"]["suggestion"]
                .as_str()
                .is_some_and(|suggestion| !suggestion.is_empty())
    }));
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic["data"]["context"]
            .as_str()
            .is_some_and(|context| context == "\\begin{equation}")
    }));

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
fn compile_tikz_basic_shapes_emits_vector_pdf_operators() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("tikz-basic.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\n\\begin{tikzpicture}\n\\draw (0,0) -- (2,1);\n\\draw (0,0) rectangle (3,2);\n\\draw (1,1) circle (0.5cm);\n\\node at (1.5,1) {TikzLabel};\n\\end{tikzpicture}\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.trim().is_empty());

    let pdf_file = dir.path().join("tikz-basic.pdf");
    let pdf_bytes = std::fs::read(&pdf_file).expect("read output pdf");
    let pdf = String::from_utf8_lossy(&pdf_bytes);
    assert!(pdf.starts_with("%PDF-1.4"), "pdf: {pdf}");
    assert!(pdf.contains("q 1 0 0 1 "), "pdf: {pdf}");
    assert!(pdf.contains(" cm\n"), "pdf: {pdf}");
    assert!(pdf.contains(" m\n"), "pdf: {pdf}");
    assert!(pdf.contains(" l\n"), "pdf: {pdf}");
    assert!(pdf.contains(" c\n"), "pdf: {pdf}");
    assert!(pdf.contains("\nS\n"), "pdf: {pdf}");
    assert!(pdf.contains("h\n"), "pdf: {pdf}");
    assert!(pdf.contains(" w\n"), "pdf: {pdf}");
    assert!(pdf.contains("BT\n"), "pdf: {pdf}");
    assert!(pdf.contains("(TikzLabel) Tj"), "pdf: {pdf}");
    assert!(pdf.contains("\nET\n"), "pdf: {pdf}");
    assert!(pdf.contains("Q\n"), "pdf: {pdf}");
    assert!(!pdf.contains("Ferritex placeholder PDF"), "pdf: {pdf}");
}

#[test]
fn compile_tikz_nested_style_transform_clip_arrow_emits_pdf_operators() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let tex_file = dir.path().join("tikz-nested.tex");
    std::fs::write(
        &tex_file,
        "\\documentclass{article}\n\\begin{document}\n\\begin{tikzpicture}\n\\draw[->] (0,0) -- (3,0);\n\\begin{scope}[xshift=10pt,yshift=5pt]\n\\fill[red] (0,0) rectangle (1,1);\n\\clip (0,0) rectangle (2,2);\n\\draw[blue] (0,0) -- (1,1);\n\\end{scope}\n\\end{tikzpicture}\n\\end{document}\n",
    )
    .expect("write input file");

    let output = ferritex_bin()
        .args(["compile", tex_file.to_str().expect("utf-8 path")])
        .output()
        .expect("failed to run ferritex");

    assert_eq!(output.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.trim().is_empty());

    let pdf_file = dir.path().join("tikz-nested.pdf");
    let pdf_bytes = std::fs::read(&pdf_file).expect("read output pdf");
    let pdf = String::from_utf8_lossy(&pdf_bytes);
    assert!(pdf.starts_with("%PDF-1.4"), "pdf: {pdf}");
    assert!(pdf.matches("q 1 0 0 1 ").count() >= 1, "pdf: {pdf}");
    assert!(pdf.matches("q\n").count() >= 1, "pdf: {pdf}");
    assert!(pdf.matches("Q\n").count() >= 2, "pdf: {pdf}");
    assert!(
        pdf.lines().any(|line| line.ends_with("10 5 cm")),
        "pdf: {pdf}"
    );
    assert!(pdf.contains("W n"), "pdf: {pdf}");
    assert!(pdf.contains("1 0 0 rg"), "pdf: {pdf}");
    assert!(
        pdf.contains("0 0 0 rg"),
        "arrowhead fill color missing: {pdf}"
    );
    assert!(pdf.contains("0 0 1 RG"), "pdf: {pdf}");
    assert!(pdf.contains("\nf\n"), "pdf: {pdf}");
    assert!(pdf.contains("\nS\n"), "pdf: {pdf}");
    assert!(
        pdf.matches("h\nf\n").count() >= 2,
        "expected at least 2 closepath+fill sequences (rectangle + arrowhead): {pdf}"
    );
    assert!(!pdf.contains("Ferritex placeholder PDF"), "pdf: {pdf}");
}

#[test]
fn corpus_tikz_basic_shapes_fixtures_emit_vector_operators() {
    let fixtures = corpus_tex_fixtures("tikz/basic-shapes");
    assert!(fixtures.len() >= 8);

    for fixture in fixtures {
        let stem = fixture
            .file_stem()
            .and_then(|stem| stem.to_str())
            .expect("utf-8 fixture stem");
        let (stderr, pdf) = compile_fixture_via_cli(&fixture, None);

        assert!(
            stderr.trim().is_empty(),
            "fixture {} stderr: {stderr}",
            fixture.display()
        );
        assert!(
            pdf.starts_with("%PDF-1.4"),
            "fixture {} pdf: {pdf}",
            fixture.display()
        );

        match stem {
            "line" => {
                assert!(
                    pdf.contains(" m\n"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
                assert!(
                    pdf.contains(" l\n"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
                assert!(
                    pdf.contains("\nS\n"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
            }
            "rectangle" => {
                assert!(
                    pdf.contains(" m\n"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
                assert!(
                    pdf.contains(" l\n"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
                assert!(
                    pdf.contains("h\n"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
                assert!(
                    pdf.contains("\nS\n"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
            }
            "circle" => {
                assert!(
                    pdf.contains(" c\n"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
                assert!(
                    pdf.contains("\nS\n"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
            }
            "ellipse" => {
                assert!(
                    pdf.contains(" m\n"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
                assert!(
                    pdf.contains(" l\n"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
                assert!(
                    pdf.contains("0 1 0 rg"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
            }
            "arc_basic" => {
                assert!(
                    pdf.contains(" c\n"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
                assert!(
                    pdf.contains("\nS\n"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
            }
            "ellipse_native" => {
                assert!(
                    pdf.contains(" c\n"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
                assert!(
                    pdf.contains("\nS\n"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
            }
            "grid_pattern" => {
                assert!(
                    pdf.contains(" m\n"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
                assert!(
                    pdf.contains(" l\n"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
                assert!(
                    pdf.contains(" c\n"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
                assert!(
                    pdf.contains("0 0 1 RG"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
                assert!(
                    pdf.contains("0 1 0 RG"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
            }
            "line_width_presets" => {
                assert!(
                    pdf.contains(" m\n"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
                assert!(
                    pdf.contains(" l\n"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
                assert!(
                    pdf.contains(" w\n"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
            }
            "text_node" => {
                assert!(
                    pdf.contains("BT\n"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
                assert!(
                    pdf.contains("(NodeText) Tj"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
                assert!(
                    pdf.contains("\nET\n"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
            }
            "path_operations" => {
                assert!(
                    pdf.contains(" m\n"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
                assert!(
                    pdf.contains(" l\n"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
                assert!(
                    pdf.contains(" c\n"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
                assert!(
                    pdf.contains("0 0 1 rg"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
            }
            "mixed_shapes" => {
                assert!(
                    pdf.contains(" m\n"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
                assert!(
                    pdf.contains(" l\n"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
                assert!(
                    pdf.contains(" c\n"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
                assert!(
                    pdf.contains("BT\n"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
                assert!(
                    pdf.contains("(NodeText) Tj"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
                assert!(
                    pdf.contains("\nET\n"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
            }
            other => panic!("unexpected tikz basic fixture stem: {other}"),
        }
    }
}

#[test]
fn corpus_tikz_nested_fixtures_emit_expected_operators() {
    let fixtures = corpus_tex_fixtures("tikz/nested-style-transform-clip-arrow");
    assert!(fixtures.len() >= 8);

    for fixture in fixtures {
        let stem = fixture
            .file_stem()
            .and_then(|stem| stem.to_str())
            .expect("utf-8 fixture stem");
        let (stderr, pdf) = compile_fixture_via_cli(&fixture, None);

        assert!(
            stderr.trim().is_empty(),
            "fixture {} stderr: {stderr}",
            fixture.display()
        );
        assert!(
            pdf.starts_with("%PDF-1.4"),
            "fixture {} pdf: {pdf}",
            fixture.display()
        );
        assert!(
            pdf.contains("q\n") || pdf.contains("q 1 0 0 1 "),
            "fixture {} pdf: {pdf}",
            fixture.display()
        );
        assert!(
            pdf.contains("Q\n"),
            "fixture {} pdf: {pdf}",
            fixture.display()
        );

        match stem {
            "scope_style_inherit" => {
                assert!(
                    pdf.contains("1 0 0 RG"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
                assert!(
                    pdf.contains("0 0 1 rg"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
            }
            "transform_shift" => {
                assert!(
                    pdf.lines().any(|line| line.ends_with("10 5 cm")),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
                assert!(
                    pdf.contains("0 0 1 RG"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
            }
            "clip_rect" => {
                assert!(
                    pdf.contains("W n"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
                assert!(
                    pdf.contains("1 0 0 rg"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
            }
            "arrow_styles" => {
                assert!(
                    pdf.contains("\nS\n"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
                assert!(
                    pdf.contains("0 0 0 rg"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
                assert!(
                    pdf.matches("h\nf\n").count() >= 2,
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
            }
            "combined_nested" => {
                assert!(
                    pdf.lines().any(|line| line.ends_with("10 5 cm")),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
                assert!(
                    pdf.contains("W n"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
                assert!(
                    pdf.contains("1 0 0 rg"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
                assert!(
                    pdf.contains("0 0 1 RG"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
            }
            "foreach_loop" => {
                assert!(
                    pdf.contains(" c\n"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
                assert!(
                    pdf.contains("0 0 1 RG"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
                assert!(
                    pdf.contains("0 1 0 rg"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
            }
            "layered_drawing" => {
                assert!(
                    pdf.contains(" m\n"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
                assert!(
                    pdf.contains(" l\n"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
                assert!(
                    pdf.contains(" cm\n"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
                assert!(
                    pdf.contains("1 1 1 rg"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
            }
            "rotate_scale" => {
                assert!(
                    pdf.contains(" cm\n"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
                assert!(
                    pdf.contains(" m\n"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
                assert!(
                    pdf.contains(" l\n"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
                assert!(
                    pdf.contains("0 0 1 RG"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
                assert!(
                    pdf.contains("0 1 0 RG"),
                    "fixture {} pdf: {pdf}",
                    fixture.display()
                );
            }
            other => panic!("unexpected tikz nested fixture stem: {other}"),
        }
    }
}

#[test]
fn corpus_partition_book_fixtures_compile_via_cli() {
    let fixtures = corpus_tex_fixtures("partition-book");
    assert!(fixtures.len() >= 3);

    for fixture in fixtures {
        let (stderr, pdf) = compile_fixture_via_cli(&fixture, Some("4"));

        assert!(
            stderr.trim().is_empty(),
            "fixture {} stderr: {stderr}",
            fixture.display()
        );
        assert!(
            pdf.starts_with("%PDF-1.4"),
            "fixture {} pdf: {pdf}",
            fixture.display()
        );
        assert!(
            pdf_page_count(&pdf) >= 2,
            "fixture {} page count: {}, pdf: {pdf}",
            fixture.display(),
            pdf_page_count(&pdf)
        );
    }
}

#[test]
fn corpus_partition_article_fixtures_compile_via_cli() {
    let fixtures = corpus_tex_fixtures("partition-article");
    assert!(fixtures.len() >= 3);

    for fixture in fixtures {
        let (stderr, pdf) = compile_fixture_via_cli(&fixture, None);

        assert!(
            stderr.trim().is_empty(),
            "fixture {} stderr: {stderr}",
            fixture.display()
        );
        assert!(
            pdf.starts_with("%PDF-1.4"),
            "fixture {} pdf: {pdf}",
            fixture.display()
        );
        assert!(
            pdf_page_count(&pdf) >= 1,
            "fixture {} page count: {}, pdf: {pdf}",
            fixture.display(),
            pdf_page_count(&pdf)
        );
    }
}

// Regression for https://github.com/RK0429/ferritex/issues/24: \section
// partitioning sliced a multicols environment across boundaries, so the last
// partition's body ended with a stray BODY_MULTICOL_END sentinel (U+E02C) that
// surfaced as "cannot be represented in WinAnsiEncoding" and a '?' glyph.
#[test]
fn multicol_article_does_not_leak_private_use_sentinels() {
    let fixture = bench_fixtures_root()
        .join("corpus")
        .join("partition-article")
        .join("multicol_article.tex");
    let (stderr, _pdf) = compile_fixture_via_cli(&fixture, None);
    assert!(
        !stderr.contains("cannot be represented in WinAnsiEncoding"),
        "multicol_article leaked a WinAnsi fallback warning: {stderr}"
    );
    assert!(
        !stderr.contains("U+E0"),
        "multicol_article leaked a private-use-area sentinel: {stderr}"
    );
}

fn corpus_tex_fixtures(subset: &str) -> Vec<PathBuf> {
    let mut fixtures = std::fs::read_dir(bench_fixtures_root().join("corpus").join(subset))
        .unwrap_or_else(|error| panic!("failed to read corpus fixtures for {subset}: {error}"))
        .map(|entry| {
            entry
                .unwrap_or_else(|error| panic!("failed to enumerate corpus fixture entry: {error}"))
                .path()
        })
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("tex"))
        .collect::<Vec<_>>();
    fixtures.sort();
    fixtures
}

fn compile_fixture_via_cli(fixture: &Path, jobs: Option<&str>) -> (String, String) {
    let output_dir = tempfile::tempdir().expect("create output tempdir");
    let mut command = ferritex_bin();
    command.args([
        "compile",
        fixture.to_str().expect("utf-8 fixture path"),
        "--output-dir",
        output_dir.path().to_str().expect("utf-8 output path"),
    ]);
    if let Some(jobs) = jobs {
        command.args(["--jobs", jobs]);
    }

    let output = command.output().expect("failed to run ferritex");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(
        output.status.code(),
        Some(0),
        "fixture {} failed, stderr: {stderr}",
        fixture.display()
    );

    let pdf_path = output_dir.path().join(format!(
        "{}.pdf",
        fixture
            .file_stem()
            .and_then(|stem| stem.to_str())
            .expect("utf-8 fixture stem")
    ));
    let pdf_bytes = std::fs::read(&pdf_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", pdf_path.display()));
    (stderr, String::from_utf8_lossy(&pdf_bytes).into_owned())
}

#[test]
fn version_flag_prints_version_info() {
    let output = ferritex_bin()
        .arg("--version")
        .output()
        .expect("failed to run ferritex");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("ferritex"));
    assert!(stdout.contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn help_flag_shows_description() {
    let output = ferritex_bin()
        .arg("--help")
        .output()
        .expect("failed to run ferritex");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("compile"),
        "help should list compile subcommand"
    );
    assert!(
        stdout.contains("Compile a LaTeX document to PDF"),
        "help should show compile description"
    );
    assert!(
        stdout.contains("A Rust-native LaTeX compiler"),
        "help should show about text"
    );
}

#[test]
fn compile_help_shows_option_descriptions() {
    let output = ferritex_bin()
        .args(["compile", "--help"])
        .output()
        .expect("failed to run ferritex");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--output-dir"),
        "compile help should show --output-dir"
    );
    assert!(
        stdout.contains("Output directory"),
        "compile help should describe --output-dir"
    );
}

#[test]
fn compile_help_warns_about_high_jobs_rss() {
    let output = ferritex_bin()
        .args(["compile", "--help"])
        .output()
        .expect("failed to run ferritex");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--jobs"), "compile help should show --jobs");
    assert!(
        stdout.contains("parallel compilation tasks"),
        "compile help should describe parallel compilation tasks"
    );
    assert!(
        stdout.contains("peak RSS"),
        "compile help should warn about peak RSS"
    );
}

#[test]
fn lsp_help_documents_jsonrpc_protocol() {
    let output = ferritex_bin()
        .args(["lsp", "--help"])
        .output()
        .expect("failed to run ferritex");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Content-Length"),
        "lsp help should describe JSON-RPC framing"
    );
    assert!(
        stdout.contains("initialize"),
        "lsp help should describe the initialize/initialized handshake"
    );
    assert!(
        stdout.contains("publishDiagnostics"),
        "lsp help should mention publishDiagnostics notifications"
    );
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

fn wait_for_exit(
    child: &mut std::process::Child,
    timeout: Duration,
) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match child.try_wait().expect("poll child status") {
            Some(status) => return Some(status),
            None => thread::sleep(Duration::from_millis(25)),
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    None
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
