mod archive;

use std::fs;
use std::path::Path;
use zed::settings::LspSettings;
use zed_extension_api::{self as zed, serde_json, LanguageServerId, Result};

const DOT_LANGUAGE_SERVER: &str = "dot-language-server";
const PREVIEW_SERVER: &str = "graphviz-preview";
const PREVIEW_REPO: &str = "gabeins/zed-graphviz";

struct DotLspExtension;

impl DotLspExtension {
    fn language_server_binary_path(
        &mut self,
        _language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<String> {
        worktree
            .which("dot-language-server")
            .ok_or(String::from("Ensure dot-language-server is in PATH"))
    }

    /// Finds the preview server on PATH, or downloads it from this
    /// extension's GitHub release. A `binary` setting for the server is
    /// handled by Zed itself before the extension is asked.
    fn preview_binary_path(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<String> {
        if let Some(path) = worktree.which(PREVIEW_SERVER) {
            return Ok(path);
        }
        // Checks for a finished installation first, so this is cheap.
        download_preview_server(language_server_id)
    }
}

fn download_preview_server(language_server_id: &LanguageServerId) -> Result<String> {
    let (os, arch) = zed::current_platform();
    let target = match (os, arch) {
        (zed::Os::Mac, zed::Architecture::Aarch64) => "aarch64-apple-darwin",
        (zed::Os::Mac, zed::Architecture::X8664) => "x86_64-apple-darwin",
        (zed::Os::Linux, zed::Architecture::Aarch64) => "aarch64-unknown-linux-gnu",
        (zed::Os::Linux, zed::Architecture::X8664) => "x86_64-unknown-linux-gnu",
        (zed::Os::Windows, zed::Architecture::X8664) => "x86_64-pc-windows-msvc",
        (os, arch) => {
            return Err(format!(
                "graphviz-preview has no prebuilt binary for {os:?} {arch:?}; \
                 build it from the server/ folder of {PREVIEW_REPO} and put it on PATH"
            ))
        }
    };
    let binary_name = match os {
        zed::Os::Windows => "graphviz-preview.exe",
        _ => "graphviz-preview",
    };

    let version = env!("CARGO_PKG_VERSION");
    let version_dir = format!("{PREVIEW_SERVER}-v{version}");
    let binary_path = format!("{version_dir}/{binary_name}");
    if archive::is_complete(Path::new(&version_dir), binary_name) {
        return Ok(binary_path);
    }

    zed::set_language_server_installation_status(
        language_server_id,
        &zed::LanguageServerInstallationStatus::CheckingForUpdate,
    );
    let tag = format!("v{version}");
    let release = zed::github_release_by_tag_name(PREVIEW_REPO, &tag)
        .map_err(|e| format!("could not find release {tag} of {PREVIEW_REPO}: {e}"))?;
    let asset_name = format!("{PREVIEW_SERVER}-{target}.tar.gz");
    let checksum_name = format!("{asset_name}.sha256");
    let find = |name: &str| {
        release
            .assets
            .iter()
            .find(|asset| asset.name == name)
            .ok_or_else(|| format!("release {tag} has no asset {name}"))
    };
    let (asset, checksum) = (find(&asset_name)?, find(&checksum_name)?);

    zed::set_language_server_installation_status(
        language_server_id,
        &zed::LanguageServerInstallationStatus::Downloading,
    );
    // Unpack into a staging folder next to the final one and move it into
    // place only once it is complete, so an interrupted install is never
    // mistaken for a finished one.
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let staging = format!("{version_dir}.staging-{unique}");
    let installed = install_verified(
        &asset.download_url,
        &asset_name,
        &checksum.download_url,
        &staging,
    )
    .and_then(|()| zed::make_file_executable(&format!("{staging}/{binary_name}")))
    .and_then(|()| {
        archive::commit_install(Path::new(&staging), Path::new(&version_dir), binary_name)
    });
    if let Err(e) = installed {
        fs::remove_dir_all(&staging).ok();
        zed::set_language_server_installation_status(
            language_server_id,
            &zed::LanguageServerInstallationStatus::Failed(e.clone()),
        );
        return Err(e);
    }

    // Remove binaries of earlier versions.
    if let Ok(entries) = fs::read_dir(".") {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(&format!("{PREVIEW_SERVER}-v")) && name != version_dir {
                fs::remove_dir_all(entry.path()).ok();
            }
        }
    }

    zed::set_language_server_installation_status(
        language_server_id,
        &zed::LanguageServerInstallationStatus::None,
    );
    Ok(binary_path)
}

/// Downloads the archive and its checksum, and unpacks the archive into
/// `dir` only when the checksum matches.
fn install_verified(
    archive_url: &str,
    archive_name: &str,
    checksum_url: &str,
    dir: &str,
) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| format!("could not create {dir}: {e}"))?;
    let archive_path = format!("{dir}/{archive_name}");
    let checksum_path = format!("{archive_path}.sha256");
    zed::download_file(
        archive_url,
        &archive_path,
        zed::DownloadedFileType::Uncompressed,
    )
    .map_err(|e| format!("could not download {archive_name}: {e}"))?;
    zed::download_file(
        checksum_url,
        &checksum_path,
        zed::DownloadedFileType::Uncompressed,
    )
    .map_err(|e| format!("could not download {archive_name}.sha256: {e}"))?;
    let checksum = fs::read_to_string(&checksum_path)
        .map_err(|e| format!("could not read {checksum_path}: {e}"))?;
    let expected = archive::parse_checksum(&checksum, archive_name)?;
    let bytes =
        fs::read(&archive_path).map_err(|e| format!("could not read {archive_path}: {e}"))?;
    archive::verify_sha256(&bytes, &expected)
        .map_err(|e| format!("refusing the downloaded {archive_name}: {e}"))?;
    archive::unpack_tar_gz(&bytes, Path::new(dir))
        .map_err(|e| format!("could not unpack {archive_name}: {e}"))?;
    fs::remove_file(&archive_path).ok();
    fs::remove_file(&checksum_path).ok();
    Ok(())
}

fn server_settings(server: &str, worktree: &zed::Worktree) -> serde_json::Value {
    LspSettings::for_worktree(server, worktree)
        .ok()
        .and_then(|lsp_settings| lsp_settings.settings.clone())
        .unwrap_or_default()
}

impl zed::Extension for DotLspExtension {
    fn new() -> Self {
        Self
    }

    fn language_server_command(
        &mut self,
        language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<zed::Command> {
        if language_server_id.as_ref() == PREVIEW_SERVER {
            // The shell environment carries the user's PATH, so a `dot`
            // installed with Homebrew or a package manager is found even
            // when Zed was started from the Dock.
            return Ok(zed::Command {
                command: self.preview_binary_path(language_server_id, worktree)?,
                args: Vec::new(),
                env: worktree.shell_env(),
            });
        }

        Ok(zed::Command {
            command: self.language_server_binary_path(language_server_id, worktree)?,
            args: vec![String::from("--stdio")],
            env: Default::default(),
        })
    }

    fn language_server_initialization_options(
        &mut self,
        language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<Option<serde_json::Value>> {
        if language_server_id.as_ref() == PREVIEW_SERVER {
            return Ok(Some(serde_json::json!({
                "graphviz-preview": server_settings(PREVIEW_SERVER, worktree)
            })));
        }
        Ok(None)
    }

    fn language_server_workspace_configuration(
        &mut self,
        language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<Option<serde_json::Value>> {
        if language_server_id.as_ref() == PREVIEW_SERVER {
            return Ok(Some(serde_json::json!({
                "graphviz-preview": server_settings(PREVIEW_SERVER, worktree)
            })));
        }

        Ok(Some(serde_json::json!({
            "dot-language-server": server_settings(DOT_LANGUAGE_SERVER, worktree)
        })))
    }
}

zed::register_extension!(DotLspExtension);
