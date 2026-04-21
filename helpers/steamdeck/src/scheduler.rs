use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedulerBackend {
    LinuxCron,
    WindowsTask,
}

#[derive(Debug, Clone)]
pub struct ScheduleStatus {
    pub installed: bool,
    pub details: String,
}

pub fn install_schedule(
    backend: SchedulerBackend,
    task_name: &str,
    binary_path: &Path,
    config_path: &Path,
    every_minutes: u32,
) -> Result<String> {
    if every_minutes == 0 {
        bail!("--every-minutes moet >= 1 zijn");
    }

    let sync_command = build_sync_command(binary_path, config_path);
    match backend {
        SchedulerBackend::LinuxCron => install_linux_cron(task_name, &sync_command, every_minutes),
        SchedulerBackend::WindowsTask => {
            install_windows_task(task_name, &sync_command, every_minutes)
        }
    }
}

pub fn scheduler_status(
    backend: SchedulerBackend,
    task_name: &str,
    binary_path: &Path,
    config_path: &Path,
) -> Result<ScheduleStatus> {
    let sync_command = build_sync_command(binary_path, config_path);
    match backend {
        SchedulerBackend::LinuxCron => linux_cron_status(task_name, &sync_command),
        SchedulerBackend::WindowsTask => windows_task_status(task_name),
    }
}

pub fn uninstall_schedule(backend: SchedulerBackend, task_name: &str) -> Result<String> {
    match backend {
        SchedulerBackend::LinuxCron => uninstall_linux_cron(task_name),
        SchedulerBackend::WindowsTask => uninstall_windows_task(task_name),
    }
}

fn install_linux_cron(task_name: &str, sync_command: &str, every_minutes: u32) -> Result<String> {
    if every_minutes > 59 {
        bail!("linux cron ondersteunt in deze helper maximaal 59 minuten interval");
    }

    let marker = cron_marker(task_name);
    let mut lines = current_crontab_lines()?;
    lines.retain(|line| !line.contains(&marker));

    let cron_expr = format!("*/{} * * * *", every_minutes);
    lines.push(format!(
        "{} {} # {}",
        cron_expr,
        sync_command,
        marker.as_str()
    ));

    write_crontab_lines(&lines)?;
    Ok(format!(
        "Cron job installed: {} {} # {}",
        cron_expr, sync_command, marker
    ))
}

fn linux_cron_status(task_name: &str, sync_command: &str) -> Result<ScheduleStatus> {
    let marker = cron_marker(task_name);
    let lines = current_crontab_lines()?;
    let maybe = lines
        .into_iter()
        .find(|line| line.contains(&marker) || line.contains(sync_command));

    if let Some(line) = maybe {
        return Ok(ScheduleStatus {
            installed: true,
            details: line,
        });
    }

    Ok(ScheduleStatus {
        installed: false,
        details: "Cron job niet gevonden".to_string(),
    })
}

fn uninstall_linux_cron(task_name: &str) -> Result<String> {
    let marker = cron_marker(task_name);
    let mut lines = current_crontab_lines()?;
    let before = lines.len();
    lines.retain(|line| !line.contains(&marker));

    if before == lines.len() {
        return Ok("Geen cron job gevonden om te verwijderen".to_string());
    }

    write_crontab_lines(&lines)?;
    Ok("Cron job verwijderd".to_string())
}

fn current_crontab_lines() -> Result<Vec<String>> {
    let output = Command::new("crontab")
        .arg("-l")
        .output()
        .context("kan crontab -l niet uitvoeren")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
        if stderr.contains("no crontab") || stderr.contains("geen crontab") {
            return Ok(Vec::new());
        }
        let code = output.status.code().unwrap_or(-1);
        bail!("crontab -l faalde (code={}): {}", code, stderr.trim());
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| line.to_string())
        .collect())
}

fn write_crontab_lines(lines: &[String]) -> Result<()> {
    let mut child = Command::new("crontab")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("kan crontab - niet starten")?;

    {
        let mut stdin = child
            .stdin
            .take()
            .context("kan stdin voor crontab niet openen")?;
        let payload = if lines.is_empty() {
            String::new()
        } else {
            format!("{}\n", lines.join("\n"))
        };
        stdin
            .write_all(payload.as_bytes())
            .context("kan crontab stdin niet schrijven")?;
    }

    let output = child
        .wait_with_output()
        .context("kan crontab - output niet lezen")?;
    if !output.status.success() {
        let code = output.status.code().unwrap_or(-1);
        bail!(
            "crontab - faalde (code={}): {}",
            code,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn install_windows_task(task_name: &str, sync_command: &str, every_minutes: u32) -> Result<String> {
    let output = Command::new("schtasks")
        .args([
            "/Create",
            "/F",
            "/SC",
            "MINUTE",
            "/MO",
            &every_minutes.to_string(),
            "/TN",
            task_name,
            "/TR",
            sync_command,
        ])
        .output()
        .context("kan schtasks /Create niet uitvoeren")?;

    if !output.status.success() {
        bail!(
            "schtasks /Create faalde: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    Ok(format!(
        "Task Scheduler job installed: {} (every {} minutes)",
        task_name, every_minutes
    ))
}

fn windows_task_status(task_name: &str) -> Result<ScheduleStatus> {
    let output = Command::new("schtasks")
        .args(["/Query", "/TN", task_name, "/V", "/FO", "LIST"])
        .output()
        .context("kan schtasks /Query niet uitvoeren")?;

    if output.status.success() {
        return Ok(ScheduleStatus {
            installed: true,
            details: String::from_utf8_lossy(&output.stdout).to_string(),
        });
    }

    let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
    if stderr.contains("cannot find the file") || stderr.contains("cannot find") {
        return Ok(ScheduleStatus {
            installed: false,
            details: "Task Scheduler job niet gevonden".to_string(),
        });
    }

    bail!("schtasks /Query faalde: {}", stderr.trim())
}

fn uninstall_windows_task(task_name: &str) -> Result<String> {
    let output = Command::new("schtasks")
        .args(["/Delete", "/F", "/TN", task_name])
        .output()
        .context("kan schtasks /Delete niet uitvoeren")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
        if stderr.contains("cannot find") {
            return Ok("Geen task gevonden om te verwijderen".to_string());
        }
        bail!("schtasks /Delete faalde: {}", stderr.trim());
    }

    Ok("Task Scheduler job verwijderd".to_string())
}

fn cron_marker(task_name: &str) -> String {
    format!("sgm-helper:{}", task_name)
}

pub fn build_sync_command(binary_path: &Path, config_path: &Path) -> String {
    format!(
        "\"{}\" --config \"{}\" sync --quiet",
        binary_path.display(),
        config_path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn builds_sync_command_with_config() {
        let cmd = build_sync_command(
            &PathBuf::from("/opt/sgm/sgm-steamdeck-helper"),
            &PathBuf::from("/opt/sgm/config.ini"),
        );
        assert!(cmd.contains("--config"));
        assert!(cmd.contains("sync --quiet"));
    }

    #[test]
    fn cron_marker_is_stable() {
        assert_eq!(
            cron_marker("SGM SteamDeck Helper Sync"),
            "sgm-helper:SGM SteamDeck Helper Sync"
        );
    }
}
