//! Remote file staging for Toutiao video uploads.

use anyhow::{Context, Result, anyhow};
use std::path::Path;
use tokio::process::Command;

use crate::credentials::ToutiaoCredentials;

#[derive(Debug, Clone)]
pub(crate) struct StagedFile {
    pub(crate) remote_path: String,
    ssh_target: String,
}

pub(crate) async fn stage_file(
    credentials: &ToutiaoCredentials,
    local_path: &Path,
) -> Result<StagedFile> {
    if !local_path.exists() {
        return Err(anyhow!("local file not found: {}", local_path.display()));
    }
    let basename = local_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("upload.bin");
    let unique = format!("{}-{basename}", uuid::Uuid::new_v4());
    let remote_dir = credentials.effective_remote_temp_dir();
    let remote_path = format!("{}/{}", remote_dir.trim_end_matches('/'), unique);
    let Some(target) = credentials.ssh_target() else {
        return Ok(StagedFile {
            remote_path: local_path.to_string_lossy().to_string(),
            ssh_target: String::new(),
        });
    };
    ssh_mkdir(credentials, &target, remote_dir).await?;
    scp_copy(credentials, local_path, &target, &remote_path).await?;
    Ok(StagedFile {
        remote_path,
        ssh_target: target,
    })
}

pub(crate) async fn cleanup_file(
    credentials: &ToutiaoCredentials,
    staged: &StagedFile,
) -> Result<()> {
    if staged.ssh_target.is_empty() {
        return Ok(());
    }
    let ps = format!(
        "powershell -NoProfile -Command \"Remove-Item -Force -ErrorAction SilentlyContinue '{}'\"",
        staged.remote_path.replace('\'', "''")
    );
    let mut cmd = ssh_command(credentials);
    cmd.arg(&staged.ssh_target).arg(ps);
    let _ = cmd.status().await;
    Ok(())
}

async fn ssh_mkdir(credentials: &ToutiaoCredentials, target: &str, dir: &str) -> Result<()> {
    let ps = format!(
        "powershell -NoProfile -Command \"New-Item -ItemType Directory -Force -Path '{}' | Out-Null\"",
        dir.replace('\'', "''")
    );
    let mut cmd = ssh_command(credentials);
    cmd.arg(target).arg(ps);
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
    credentials: &ToutiaoCredentials,
    local: &Path,
    target: &str,
    remote: &str,
) -> Result<()> {
    let mut cmd = if credentials.ssh_password.is_empty() {
        Command::new("scp")
    } else {
        let mut c = Command::new("sshpass");
        c.arg("-p").arg(&credentials.ssh_password).arg("scp");
        c
    };
    if let Some(port) = credentials.ssh_port {
        cmd.arg("-P").arg(port.to_string());
    }
    cmd.arg("-q")
        .arg("-o")
        .arg("StrictHostKeyChecking=accept-new");
    if credentials.ssh_password.is_empty() {
        cmd.arg("-o").arg("BatchMode=yes");
    }
    cmd.arg(local).arg(format!("{target}:{remote}"));
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

fn ssh_command(credentials: &ToutiaoCredentials) -> Command {
    let mut cmd = if credentials.ssh_password.is_empty() {
        Command::new("ssh")
    } else {
        let mut c = Command::new("sshpass");
        c.arg("-p").arg(&credentials.ssh_password).arg("ssh");
        c
    };
    if let Some(port) = credentials.ssh_port {
        cmd.arg("-p").arg(port.to_string());
    }
    cmd.arg("-o").arg("StrictHostKeyChecking=accept-new");
    if credentials.ssh_password.is_empty() {
        cmd.arg("-o").arg("BatchMode=yes");
    }
    cmd
}
