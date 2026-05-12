use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde_json::json;

pub const BUILTIN_BASIC_ASSET_BUNDLE_ID: &str = "builtin:basic";
const BUILTIN_BASIC_ASSET_BUNDLE_VERSION: &str = "0.1.0";
const BUILTIN_BASIC_BUNDLED_TEX: &str = "Bundled from built-in asset bundle.\n";
const BUILTIN_BASIC_ASSET_INDEX_PATH: &str = "asset-index.json";
const BUILTIN_BASIC_CMR10_TFM_PATH: &str = "texmf/fonts/tfm/public/cm/cmr10.tfm";
const TAR_BLOCK_SIZE: usize = 512;
const MAX_ARCHIVE_DECOMPRESSED_BYTES: usize = 64 * 1024 * 1024;
const ARCHIVE_DECOMPRESSION_TIMEOUT: Duration = Duration::from_secs(30);

/// CLI から受け取る compile サブコマンド引数
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileArgs {
    pub input_file: PathBuf,
    pub output_dir: Option<PathBuf>,
    pub jobname: Option<String>,
    pub jobs: Option<usize>,
    pub overlay_roots: Vec<PathBuf>,
    pub no_cache: bool,
    pub asset_bundle: Option<PathBuf>,
    pub reproducible: bool,
    pub interaction: Option<CompileInteraction>,
    pub synctex: bool,
    pub trace_font_tasks: bool,
    pub shell_escape: bool,
    pub no_shell_escape: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompileInteraction {
    Nonstopmode,
    Batchmode,
    Scrollmode,
    Errorstopmode,
}

/// CLI フラグから正規化されたランタイムオプション
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeOptions {
    pub input_file: PathBuf,
    pub output_dir: PathBuf,
    pub jobname: String,
    pub parallelism: usize,
    pub overlay_roots: Vec<PathBuf>,
    pub no_cache: bool,
    pub asset_bundle: Option<PathBuf>,
    pub host_font_fallback: bool,
    pub host_font_roots: Vec<PathBuf>,
    pub interaction_mode: InteractionMode,
    pub synctex: bool,
    pub trace_font_tasks: bool,
    pub shell_escape: ShellEscapeMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteractionMode {
    Nonstopmode,
    Batchmode,
    Scrollmode,
    Errorstopmode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellEscapeMode {
    /// Shell escape is explicitly disabled.
    Disabled,
    /// The CLI default when neither `--shell-escape` nor `--no-shell-escape`
    /// is passed.
    ///
    /// Ferritex currently treats this as no shell escape support; only
    /// [`ShellEscapeMode::Enabled`] allows shell escape execution.
    Restricted,
    /// Shell escape is explicitly enabled.
    Enabled,
}

impl RuntimeOptions {
    pub fn from_compile_args(args: &CompileArgs) -> Self {
        let host_font_fallback = !args.reproducible;
        Self {
            input_file: args.input_file.clone(),
            output_dir: args
                .output_dir
                .clone()
                .unwrap_or_else(|| default_output_dir(&args.input_file)),
            jobname: args
                .jobname
                .clone()
                .unwrap_or_else(|| derive_jobname(&args.input_file)),
            parallelism: args.jobs.unwrap_or_else(default_parallelism).max(1),
            overlay_roots: args.overlay_roots.clone(),
            no_cache: args.no_cache,
            asset_bundle: args.asset_bundle.as_deref().map(resolve_asset_bundle_ref),
            host_font_fallback,
            host_font_roots: if host_font_fallback {
                default_host_font_roots()
            } else {
                Vec::new()
            },
            interaction_mode: match args.interaction.unwrap_or(CompileInteraction::Nonstopmode) {
                CompileInteraction::Nonstopmode => InteractionMode::Nonstopmode,
                CompileInteraction::Batchmode => InteractionMode::Batchmode,
                CompileInteraction::Scrollmode => InteractionMode::Scrollmode,
                CompileInteraction::Errorstopmode => InteractionMode::Errorstopmode,
            },
            synctex: args.synctex,
            trace_font_tasks: args.trace_font_tasks,
            shell_escape: normalize_shell_escape(args.shell_escape, args.no_shell_escape),
        }
    }

    pub fn for_lsp(
        input_file: PathBuf,
        output_dir: Option<PathBuf>,
        jobname: Option<String>,
    ) -> Self {
        Self {
            output_dir: output_dir.unwrap_or_else(|| default_output_dir(&input_file)),
            jobname: jobname.unwrap_or_else(|| derive_jobname(&input_file)),
            input_file,
            parallelism: default_parallelism(),
            overlay_roots: Vec::new(),
            no_cache: false,
            asset_bundle: default_lsp_asset_bundle(),
            host_font_fallback: true,
            host_font_roots: default_host_font_roots(),
            interaction_mode: InteractionMode::Nonstopmode,
            synctex: false,
            trace_font_tasks: false,
            shell_escape: ShellEscapeMode::Disabled,
        }
    }
}

pub fn default_lsp_asset_bundle() -> Option<PathBuf> {
    Some(resolve_asset_bundle_ref(Path::new(
        BUILTIN_BASIC_ASSET_BUNDLE_ID,
    )))
}

pub fn resolve_asset_bundle_ref(bundle_ref: &Path) -> PathBuf {
    if bundle_ref == Path::new(BUILTIN_BASIC_ASSET_BUNDLE_ID) {
        materialize_builtin_basic_asset_bundle()
    } else if is_asset_bundle_archive(bundle_ref) {
        match materialize_archive_asset_bundle(bundle_ref) {
            Ok(path) => path,
            Err(reason) => materialize_archive_error_bundle(bundle_ref, &reason)
                .unwrap_or_else(|_| bundle_ref.to_path_buf()),
        }
    } else {
        bundle_ref.to_path_buf()
    }
}

fn is_asset_bundle_archive(bundle_ref: &Path) -> bool {
    let value = bundle_ref.to_string_lossy();
    value.ends_with(".tar.gz") || value.ends_with(".tgz")
}

fn materialize_archive_asset_bundle(bundle_ref: &Path) -> Result<PathBuf, String> {
    if !bundle_ref.is_file() {
        return Err(format!(
            "archive does not exist at {}",
            bundle_ref.display()
        ));
    }

    let staging_root =
        std::env::temp_dir().join(format!("ferritex-asset-bundle-{}", unique_temp_suffix()));
    let _ = std::fs::remove_dir_all(&staging_root);
    std::fs::create_dir_all(&staging_root).map_err(|error| {
        format!(
            "failed to prepare archive staging root {}: {error}",
            staging_root.display()
        )
    })?;

    let tar_bytes = decompress_gzip_archive(bundle_ref).inspect_err(|_| {
        let _ = std::fs::remove_dir_all(&staging_root);
    })?;
    extract_checked_tar(&tar_bytes, &staging_root).inspect_err(|_| {
        let _ = std::fs::remove_dir_all(&staging_root);
    })?;

    let Some(extracted_root) = find_extracted_bundle_root(&staging_root) else {
        let _ = std::fs::remove_dir_all(&staging_root);
        return Err("archive does not contain a single asset bundle root".to_string());
    };

    Ok(extracted_root)
}

fn decompress_gzip_archive(bundle_ref: &Path) -> Result<Vec<u8>, String> {
    decompress_gzip_archive_with_limits(
        bundle_ref,
        MAX_ARCHIVE_DECOMPRESSED_BYTES,
        ARCHIVE_DECOMPRESSION_TIMEOUT,
    )
}

fn decompress_gzip_archive_with_limits(
    bundle_ref: &Path,
    max_bytes: usize,
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    let mut child = Command::new("gzip")
        .arg("-cd")
        .arg(bundle_ref)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("failed to run gzip: {error}"))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "failed to capture gzip stdout".to_string())?;
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let result = read_bounded(stdout, max_bytes);
        let _ = sender.send(result);
    });

    let deadline = Instant::now() + timeout;
    let mut output_result = None;
    loop {
        if output_result.is_none() {
            match receiver.try_recv() {
                Ok(result) => {
                    if result.is_err() {
                        let _ = child.kill();
                    }
                    output_result = Some(result);
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    output_result = Some(Err(
                        "failed to read decompressed archive from gzip".to_string()
                    ));
                }
            }
        }

        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("failed to wait for gzip: {error}"))?
        {
            let bytes = match output_result {
                Some(result) => result?,
                None => receiver
                    .recv()
                    .map_err(|_| "failed to read decompressed archive from gzip".to_string())??,
            };
            if !status.success() {
                return Err("failed to decompress archive with gzip".to_string());
            }
            return Ok(bytes);
        }

        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "archive decompression exceeded {} seconds",
                timeout.as_secs()
            ));
        }

        thread::sleep(Duration::from_millis(10));
    }
}

fn read_bounded(mut reader: impl Read, max_bytes: usize) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|error| format!("failed to read decompressed archive: {error}"))?;
        if count == 0 {
            return Ok(bytes);
        }
        if bytes.len().saturating_add(count) > max_bytes {
            return Err(format!(
                "archive decompressed size exceeds {} bytes",
                max_bytes
            ));
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
}

fn extract_checked_tar(bytes: &[u8], staging_root: &Path) -> Result<(), String> {
    let canonical_staging = staging_root.canonicalize().map_err(|error| {
        format!(
            "failed to canonicalize archive staging root {}: {error}",
            staging_root.display()
        )
    })?;
    let mut offset = 0;
    while offset + TAR_BLOCK_SIZE <= bytes.len() {
        let header = &bytes[offset..offset + TAR_BLOCK_SIZE];
        offset += TAR_BLOCK_SIZE;
        if header.iter().all(|byte| *byte == 0) {
            return Ok(());
        }

        let path = tar_entry_path(header)?;
        let size = parse_tar_octal(&header[124..136])?;
        let entry_type = header[156];
        let data_end = offset
            .checked_add(size)
            .ok_or_else(|| format!("archive entry {} is too large", path.display()))?;
        if data_end > bytes.len() {
            return Err(format!("archive entry {} is truncated", path.display()));
        }

        let destination = checked_archive_destination(&canonical_staging, &path)?;
        match entry_type {
            0 | b'0' => {
                if let Some(parent) = destination.parent() {
                    std::fs::create_dir_all(parent).map_err(|error| {
                        format!(
                            "failed to create archive entry directory {}: {error}",
                            parent.display()
                        )
                    })?;
                    verify_under_root(parent, &canonical_staging)?;
                }
                let mut file = std::fs::File::create(&destination).map_err(|error| {
                    format!(
                        "failed to create archive entry {}: {error}",
                        destination.display()
                    )
                })?;
                file.write_all(&bytes[offset..data_end]).map_err(|error| {
                    format!(
                        "failed to write archive entry {}: {error}",
                        destination.display()
                    )
                })?;
            }
            b'5' => {
                std::fs::create_dir_all(&destination).map_err(|error| {
                    format!(
                        "failed to create archive directory {}: {error}",
                        destination.display()
                    )
                })?;
                verify_under_root(&destination, &canonical_staging)?;
            }
            b'x' | b'g' => {}
            other => {
                return Err(format!(
                    "unsupported archive entry type `{}` for {}",
                    other as char,
                    path.display()
                ));
            }
        }

        offset = align_tar_offset(data_end)?;
    }

    Err("archive ended before the tar end marker".to_string())
}

fn tar_entry_path(header: &[u8]) -> Result<PathBuf, String> {
    let name = tar_string(&header[0..100])?;
    if name.is_empty() {
        return Err("archive entry has an empty path".to_string());
    }
    let prefix = tar_string(&header[345..500])?;
    let path = if prefix.is_empty() {
        PathBuf::from(name)
    } else {
        PathBuf::from(prefix).join(name)
    };
    validate_archive_relative_path(&path)?;
    Ok(path)
}

fn tar_string(bytes: &[u8]) -> Result<String, String> {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    std::str::from_utf8(&bytes[..end])
        .map(str::to_string)
        .map_err(|error| format!("archive entry path is not UTF-8: {error}"))
}

fn parse_tar_octal(bytes: &[u8]) -> Result<usize, String> {
    let value = bytes
        .iter()
        .copied()
        .take_while(|byte| *byte != 0)
        .filter(|byte| *byte != b' ')
        .collect::<Vec<_>>();
    let value = std::str::from_utf8(&value)
        .map_err(|error| format!("archive entry size is not UTF-8: {error}"))?;
    usize::from_str_radix(value.trim(), 8)
        .map_err(|error| format!("archive entry has invalid size `{value}`: {error}"))
}

fn validate_archive_relative_path(path: &Path) -> Result<(), String> {
    if path.is_absolute() {
        return Err(format!(
            "archive entry path `{}` must be relative",
            path.display()
        ));
    }
    if path.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir
                | std::path::Component::RootDir
                | std::path::Component::Prefix(_)
        )
    }) {
        return Err(format!(
            "archive entry path `{}` must not escape the bundle root",
            path.display()
        ));
    }
    Ok(())
}

fn checked_archive_destination(root: &Path, relative_path: &Path) -> Result<PathBuf, String> {
    validate_archive_relative_path(relative_path)?;
    let destination = root.join(relative_path);
    if !destination.starts_with(root) {
        return Err(format!(
            "archive entry destination `{}` escapes staging root",
            destination.display()
        ));
    }
    Ok(destination)
}

fn verify_under_root(path: &Path, root: &Path) -> Result<(), String> {
    let canonical = path.canonicalize().map_err(|error| {
        format!(
            "failed to canonicalize archive destination {}: {error}",
            path.display()
        )
    })?;
    if canonical.starts_with(root) {
        Ok(())
    } else {
        Err(format!(
            "archive destination `{}` escapes staging root",
            canonical.display()
        ))
    }
}

fn align_tar_offset(offset: usize) -> Result<usize, String> {
    offset
        .checked_add(TAR_BLOCK_SIZE - 1)
        .map(|value| value / TAR_BLOCK_SIZE * TAR_BLOCK_SIZE)
        .ok_or_else(|| "archive offset overflow".to_string())
}

fn unique_temp_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!("{}-{nanos}", std::process::id())
}

fn materialize_archive_error_bundle(bundle_ref: &Path, reason: &str) -> std::io::Result<PathBuf> {
    let root = std::env::temp_dir().join(format!(
        "ferritex-invalid-asset-bundle-{}",
        unique_temp_suffix()
    ));
    std::fs::create_dir_all(&root)?;
    let message = format!(
        "archive extraction failed for {}: {reason}",
        bundle_ref.display()
    );
    std::fs::write(
        root.join("manifest.json"),
        serde_json::to_vec(&message).expect("serialize archive extraction error"),
    )?;
    Ok(root)
}

fn find_extracted_bundle_root(staging_root: &Path) -> Option<PathBuf> {
    if staging_root.join("manifest.json").is_file()
        && staging_root.join("asset-index.json").is_file()
    {
        return Some(staging_root.to_path_buf());
    }

    let mut candidates = std::fs::read_dir(staging_root)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_dir()
                && path.join("manifest.json").is_file()
                && path.join("asset-index.json").is_file()
        })
        .collect::<Vec<_>>();

    if candidates.len() == 1 {
        candidates.pop()
    } else {
        None
    }
}

fn default_output_dir(input_file: &Path) -> PathBuf {
    input_file
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn derive_jobname(input_file: &Path) -> String {
    input_file
        .file_stem()
        .or_else(|| input_file.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "texput".to_string())
}

fn normalize_shell_escape(shell_escape: bool, no_shell_escape: bool) -> ShellEscapeMode {
    if no_shell_escape {
        ShellEscapeMode::Disabled
    } else if shell_escape {
        ShellEscapeMode::Enabled
    } else {
        ShellEscapeMode::Restricted
    }
}

fn default_parallelism() -> usize {
    std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .max(1)
}

fn default_host_font_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    #[cfg(target_os = "macos")]
    {
        roots.push(PathBuf::from("/System/Library/Fonts"));
        roots.push(PathBuf::from("/System/Library/AssetsV2"));
        roots.push(PathBuf::from("/Library/Fonts"));
        if let Some(home) = home_dir() {
            roots.push(home.join("Library/Fonts"));
        }
    }

    #[cfg(target_os = "linux")]
    {
        roots.push(PathBuf::from("/usr/share/fonts"));
        roots.push(PathBuf::from("/usr/local/share/fonts"));
        if let Some(home) = home_dir() {
            roots.push(home.join(".fonts"));
            roots.push(home.join(".local/share/fonts"));
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Some(windir) = std::env::var_os("WINDIR") {
            roots.push(PathBuf::from(windir).join("Fonts"));
        } else {
            roots.push(PathBuf::from(r"C:\Windows\Fonts"));
        }
        if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
            roots.push(PathBuf::from(local_app_data).join("Microsoft/Windows/Fonts"));
        }
    }

    roots.sort();
    roots.dedup();
    roots
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn materialize_builtin_basic_asset_bundle() -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "ferritex-builtin-bundles/basic-{BUILTIN_BASIC_ASSET_BUNDLE_VERSION}"
    ));
    let texmf_root = root.join("texmf");
    let manifest_path = root.join("manifest.json");
    let asset_index_path = root.join(BUILTIN_BASIC_ASSET_INDEX_PATH);
    let bundled_tex_path = texmf_root.join("bundled.tex");
    let cmr10_tfm_path = root.join(BUILTIN_BASIC_CMR10_TFM_PATH);
    let manifest = json!({
        "name": "basic",
        "version": BUILTIN_BASIC_ASSET_BUNDLE_VERSION,
        "min_ferritex_version": "0.1.0",
        "format_version": 1,
        "asset_index_path": BUILTIN_BASIC_ASSET_INDEX_PATH,
    });
    let asset_index = json!({
        "tex_inputs": {
            "bundled.tex": "texmf/bundled.tex",
        },
        "packages": {},
        "opentype_fonts": {},
        "tfm_fonts": {
            "cmr10": BUILTIN_BASIC_CMR10_TFM_PATH,
        },
        "default_opentype_fonts": [],
    });

    let _ = std::fs::create_dir_all(&texmf_root);
    let _ = std::fs::create_dir_all(cmr10_tfm_path.parent().unwrap_or(&root));
    let _ = std::fs::write(
        &manifest_path,
        serde_json::to_vec(&manifest).unwrap_or_default(),
    );
    let _ = std::fs::write(
        &asset_index_path,
        serde_json::to_vec(&asset_index).unwrap_or_default(),
    );
    let _ = std::fs::write(&bundled_tex_path, BUILTIN_BASIC_BUNDLED_TEX);
    let _ = std::fs::write(&cmr10_tfm_path, builtin_basic_cmr10_tfm());

    root
}

fn builtin_basic_cmr10_tfm() -> Vec<u8> {
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

#[cfg(test)]
mod tests {
    use std::{
        io::Write,
        path::{Path, PathBuf},
        process::{Command, Stdio},
    };

    use super::{
        default_lsp_asset_bundle, resolve_asset_bundle_ref, CompileArgs, CompileInteraction,
        InteractionMode, RuntimeOptions, ShellEscapeMode, BUILTIN_BASIC_ASSET_BUNDLE_ID,
    };

    fn compile_args(input_file: impl Into<PathBuf>) -> CompileArgs {
        CompileArgs {
            input_file: input_file.into(),
            output_dir: None,
            jobname: None,
            jobs: None,
            overlay_roots: Vec::new(),
            no_cache: false,
            asset_bundle: None,
            reproducible: false,
            interaction: None,
            synctex: false,
            trace_font_tasks: false,
            shell_escape: false,
            no_shell_escape: false,
        }
    }

    fn tar_header(path: &str, entry_type: u8, size: usize) -> [u8; super::TAR_BLOCK_SIZE] {
        let mut header = [0_u8; super::TAR_BLOCK_SIZE];
        let path_bytes = path.as_bytes();
        header[..path_bytes.len()].copy_from_slice(path_bytes);
        header[100..108].copy_from_slice(b"0000777\0");
        header[108..116].copy_from_slice(b"0000000\0");
        header[116..124].copy_from_slice(b"0000000\0");
        let size = format!("{size:011o}\0");
        header[124..136].copy_from_slice(size.as_bytes());
        header[136..148].copy_from_slice(b"00000000000\0");
        header[148..156].fill(b' ');
        header[156] = entry_type;
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        let checksum = header.iter().map(|byte| u32::from(*byte)).sum::<u32>();
        let checksum = format!("{checksum:06o}\0 ");
        header[148..156].copy_from_slice(checksum.as_bytes());
        header
    }

    fn tar_archive(entries: &[(&str, u8, &[u8])]) -> Vec<u8> {
        let mut archive = Vec::new();
        for (path, entry_type, content) in entries {
            archive.extend_from_slice(&tar_header(path, *entry_type, content.len()));
            archive.extend_from_slice(content);
            let padding = (super::TAR_BLOCK_SIZE - content.len() % super::TAR_BLOCK_SIZE)
                % super::TAR_BLOCK_SIZE;
            archive.extend(std::iter::repeat(0).take(padding));
        }
        archive.extend_from_slice(&[0_u8; super::TAR_BLOCK_SIZE]);
        archive.extend_from_slice(&[0_u8; super::TAR_BLOCK_SIZE]);
        archive
    }

    fn gzip_bytes(input: &[u8]) -> Vec<u8> {
        let mut child = Command::new("gzip")
            .arg("-n")
            .arg("-c")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn gzip");
        let mut stdin = child.stdin.take().expect("gzip stdin");
        stdin.write_all(input).expect("write gzip stdin");
        drop(stdin);
        let output = child.wait_with_output().expect("wait gzip");
        assert!(
            output.status.success(),
            "gzip failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }

    fn write_archive(path: &Path, entries: &[(&str, u8, &[u8])]) {
        std::fs::write(path, gzip_bytes(&tar_archive(entries))).expect("write archive");
    }

    #[test]
    fn derives_jobname_and_output_dir_defaults_from_input_file() {
        let args = compile_args(PathBuf::from("chapters/main.tex"));

        let options = RuntimeOptions::from_compile_args(&args);

        assert_eq!(options.jobname, "main");
        assert_eq!(options.output_dir, PathBuf::from("chapters"));
        assert_eq!(options.parallelism, super::default_parallelism());
        assert_eq!(options.interaction_mode, InteractionMode::Nonstopmode);
        assert_eq!(options.shell_escape, ShellEscapeMode::Restricted);
    }

    #[test]
    fn preserves_explicit_flags_when_normalizing() {
        let mut args = compile_args(PathBuf::from("input.tex"));
        args.output_dir = Some(PathBuf::from("build"));
        args.jobname = Some("custom-job".to_string());
        args.jobs = Some(4);
        args.overlay_roots = vec![PathBuf::from("shared"), PathBuf::from("vendor/texmf")];
        args.no_cache = true;
        args.asset_bundle = Some(PathBuf::from("bundle"));
        args.interaction = Some(CompileInteraction::Batchmode);
        args.synctex = true;
        args.trace_font_tasks = true;
        args.shell_escape = true;

        let options = RuntimeOptions::from_compile_args(&args);

        assert_eq!(options.output_dir, PathBuf::from("build"));
        assert_eq!(options.jobname, "custom-job");
        assert_eq!(options.parallelism, 4);
        assert_eq!(
            options.overlay_roots,
            vec![PathBuf::from("shared"), PathBuf::from("vendor/texmf")]
        );
        assert!(options.no_cache);
        assert_eq!(options.asset_bundle, Some(PathBuf::from("bundle")));
        assert!(options.host_font_fallback);
        assert_eq!(options.host_font_roots, super::default_host_font_roots());
        assert_eq!(options.interaction_mode, InteractionMode::Batchmode);
        assert!(options.synctex);
        assert!(options.trace_font_tasks);
        assert_eq!(options.shell_escape, ShellEscapeMode::Enabled);
    }

    #[test]
    fn maps_all_interaction_modes() {
        let expectations = [
            (
                CompileInteraction::Nonstopmode,
                InteractionMode::Nonstopmode,
            ),
            (CompileInteraction::Batchmode, InteractionMode::Batchmode),
            (CompileInteraction::Scrollmode, InteractionMode::Scrollmode),
            (
                CompileInteraction::Errorstopmode,
                InteractionMode::Errorstopmode,
            ),
        ];

        for (input_mode, expected_mode) in expectations {
            let mut args = compile_args(PathBuf::from("input.tex"));
            args.interaction = Some(input_mode);

            let options = RuntimeOptions::from_compile_args(&args);

            assert_eq!(options.interaction_mode, expected_mode);
        }
    }

    #[test]
    fn no_shell_escape_flag_overrides_shell_escape() {
        let mut args = compile_args(PathBuf::from("input.tex"));
        args.shell_escape = true;
        args.no_shell_escape = true;

        let options = RuntimeOptions::from_compile_args(&args);

        assert_eq!(options.shell_escape, ShellEscapeMode::Disabled);
    }

    #[test]
    fn lsp_defaults_follow_requirement_profile() {
        let options = RuntimeOptions::for_lsp(PathBuf::from("chapters/main.tex"), None, None);

        assert_eq!(options.input_file, PathBuf::from("chapters/main.tex"));
        assert_eq!(options.output_dir, PathBuf::from("chapters"));
        assert_eq!(options.jobname, "main");
        assert_eq!(options.parallelism, super::default_parallelism());
        assert!(!options.no_cache);
        let bundle_path = options.asset_bundle.expect("lsp bundle path");
        assert!(bundle_path.join("manifest.json").exists());
        assert!(bundle_path
            .join(super::BUILTIN_BASIC_ASSET_INDEX_PATH)
            .exists());
        assert!(bundle_path.join("texmf/bundled.tex").exists());
        assert!(bundle_path
            .join(super::BUILTIN_BASIC_CMR10_TFM_PATH)
            .exists());
        assert!(options.host_font_fallback);
        assert_eq!(options.host_font_roots, super::default_host_font_roots());
        assert_eq!(options.interaction_mode, InteractionMode::Nonstopmode);
        assert!(!options.synctex);
        assert!(!options.trace_font_tasks);
        assert_eq!(options.shell_escape, ShellEscapeMode::Disabled);
    }

    #[test]
    fn reproducible_mode_disables_host_font_fallback() {
        let mut args = compile_args(PathBuf::from("input.tex"));
        args.reproducible = true;

        let options = RuntimeOptions::from_compile_args(&args);

        assert!(!options.host_font_fallback);
        assert!(options.host_font_roots.is_empty());
    }

    #[test]
    fn resolves_builtin_asset_bundle_identifier_to_materialized_bundle() {
        let resolved =
            resolve_asset_bundle_ref(PathBuf::from(BUILTIN_BASIC_ASSET_BUNDLE_ID).as_path());

        assert!(resolved.join("manifest.json").exists());
        assert!(resolved
            .join(super::BUILTIN_BASIC_ASSET_INDEX_PATH)
            .exists());
        assert!(resolved.join("texmf/bundled.tex").exists());
        assert!(resolved.join(super::BUILTIN_BASIC_CMR10_TFM_PATH).exists());
    }

    #[test]
    fn rejects_malicious_asset_bundle_archive_entries() {
        let cases = [
            (
                "../evil",
                b'0',
                b"evil".as_slice(),
                "must not escape the bundle root",
            ),
            ("/absolute", b'0', b"evil".as_slice(), "must be relative"),
            (
                "FTX-ASSET-BUNDLE-001/link",
                b'2',
                b"target".as_slice(),
                "unsupported archive entry type",
            ),
            (
                "FTX-ASSET-BUNDLE-001/hardlink",
                b'1',
                b"target".as_slice(),
                "unsupported archive entry type",
            ),
            (
                "FTX-ASSET-BUNDLE-001/device",
                b'3',
                b"".as_slice(),
                "unsupported archive entry type",
            ),
        ];

        for (entry_path, entry_type, content, expected_error) in cases {
            let temp_dir = tempfile::tempdir().expect("create tempdir");
            let archive_path = temp_dir.path().join("FTX-ASSET-BUNDLE-001.tar.gz");
            write_archive(&archive_path, &[(entry_path, entry_type, content)]);

            let resolved = resolve_asset_bundle_ref(&archive_path);

            assert_ne!(resolved, archive_path);
            let manifest = std::fs::read_to_string(resolved.join("manifest.json"))
                .expect("read error manifest");
            assert!(manifest.contains("archive extraction failed"));
            assert!(
                manifest.contains(expected_error),
                "error manifest should contain `{expected_error}` but was {manifest}"
            );
        }
    }

    #[test]
    fn rejects_asset_bundle_archive_when_decompressed_size_exceeds_limit() {
        let temp_dir = tempfile::tempdir().expect("create tempdir");
        let archive_path = temp_dir.path().join("FTX-ASSET-BUNDLE-001.tar.gz");
        write_archive(
            &archive_path,
            &[("FTX-ASSET-BUNDLE-001/manifest.json", b'0', b"{}".as_slice())],
        );

        let error = super::decompress_gzip_archive_with_limits(
            &archive_path,
            128,
            std::time::Duration::from_secs(5),
        )
        .expect_err("archive should exceed decompressed size limit");

        assert!(
            error.contains("archive decompressed size exceeds 128 bytes"),
            "error should report the direct size limit but was {error}"
        );
    }

    #[test]
    fn lsp_default_bundle_matches_builtin_materialization() {
        let bundle = default_lsp_asset_bundle().expect("default bundle");

        assert!(bundle.join("manifest.json").exists());
        assert!(bundle.join(super::BUILTIN_BASIC_ASSET_INDEX_PATH).exists());
        assert!(bundle.join("texmf/bundled.tex").exists());
        assert!(bundle.join(super::BUILTIN_BASIC_CMR10_TFM_PATH).exists());
    }
}
