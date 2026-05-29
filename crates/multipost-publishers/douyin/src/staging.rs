//! SCP-based file staging.
//!
//! Douyin's upload uses `DOM.setFileInputFiles`, which takes a path on the
//! Chrome host's filesystem. When Chrome runs on a different machine than
//! multipost-server, we SCP the video over first.
//!
//! Auth: shells out to the system `scp` / `ssh` binaries, so it uses the
//! current user's ssh-agent / keys / config. We don't accept passwords.
//! Set up key-based auth (`ssh-copy-id`) on the Chrome host once.

use anyhow::{Context, Result, anyhow};
use std::path::Path;
use tokio::process::Command;

use crate::credentials::DouyinCredentials;

/// Result of staging a local file onto the Chrome host.
#[derive(Debug, Clone)]
pub struct StagedFile {
    /// Path on the Chrome host (what we hand to `DOM.setFileInputFiles`).
    pub remote_path: String,
    /// SSH target the file was staged onto. Empty when Chrome is local.
    pub ssh_target: String,
}

/// Copy `local_path` to the Chrome host's `remote_temp_dir`. Uses a random
/// filename so concurrent uploads don't clobber each other.
///
/// When `creds.ssh_host` is empty we skip SSH entirely and just verify the
/// file exists locally — Chrome can read the local path directly.
pub async fn stage_file(creds: &DouyinCredentials, local_path: &Path) -> Result<StagedFile> {
    if !local_path.exists() {
        return Err(anyhow!("local file not found: {}", local_path.display()));
    }
    let basename = local_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("upload.bin");
    let unique = format!("{}-{basename}", uuid::Uuid::new_v4());
    let remote_dir = creds.effective_remote_temp_dir();
    let remote_path = format!("{}/{}", remote_dir.trim_end_matches('/'), unique);

    let Some(target) = creds.ssh_target() else {
        // Local Chrome — no copy, just return the local path.
        return Ok(StagedFile {
            remote_path: local_path.to_string_lossy().to_string(),
            ssh_target: String::new(),
        });
    };

    ssh_mkdir(creds, &target, remote_dir)
        .await
        .context("create remote temp dir")?;
    scp_copy(creds, local_path, &target, &remote_path)
        .await
        .context("scp video to chrome host")?;
    Ok(StagedFile {
        remote_path,
        ssh_target: target,
    })
}

/// Best-effort cleanup of a staged file.
pub async fn cleanup_file(creds: &DouyinCredentials, staged: &StagedFile) -> Result<()> {
    if staged.ssh_target.is_empty() {
        return Ok(()); // never SCP'd
    }
    let mut cmd = Command::new("ssh");
    if let Some(port) = creds.ssh_port {
        cmd.arg("-p").arg(port.to_string());
    }
    let ps = format!(
        "powershell -NoProfile -Command \"Remove-Item -Force -ErrorAction SilentlyContinue '{}'\"",
        staged.remote_path.replace('\'', "''")
    );
    cmd.arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("StrictHostKeyChecking=accept-new")
        .arg(&staged.ssh_target)
        .arg(ps);
    let status = cmd.status().await.context("spawn ssh rm")?;
    if !status.success() {
        return Err(anyhow!("ssh rm exited with {status}"));
    }
    Ok(())
}

async fn ssh_mkdir(creds: &DouyinCredentials, target: &str, dir: &str) -> Result<()> {
    // Use PowerShell's `New-Item -Force` — works whether the remote default
    // shell is cmd.exe (Windows) or sh (Linux with pwsh installed). For
    // Linux hosts without pwsh we'd need a different path; left as TODO until
    // we add a non-Windows Chrome host.
    let mut cmd = Command::new("ssh");
    if let Some(port) = creds.ssh_port {
        cmd.arg("-p").arg(port.to_string());
    }
    let ps = format!(
        "powershell -NoProfile -Command \"New-Item -ItemType Directory -Force -Path '{}' | Out-Null\"",
        dir.replace('\'', "''")
    );
    cmd.arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("StrictHostKeyChecking=accept-new")
        .arg(target)
        .arg(ps);
    let out = cmd.output().await.context("spawn ssh mkdir")?;
    if !out.status.success() {
        return Err(anyhow!(
            "ssh mkdir failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(())
}

async fn scp_copy(
    creds: &DouyinCredentials,
    local: &Path,
    target: &str,
    remote: &str,
) -> Result<()> {
    let mut cmd = Command::new("scp");
    if let Some(port) = creds.ssh_port {
        cmd.arg("-P").arg(port.to_string());
    }
    cmd.arg("-q")
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("StrictHostKeyChecking=accept-new")
        .arg(local)
        .arg(format!("{target}:{remote}"));
    let out = cmd.output().await.context("spawn scp")?;
    if !out.status.success() {
        return Err(anyhow!(
            "scp failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(())
}
