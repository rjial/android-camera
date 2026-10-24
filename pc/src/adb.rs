//! USB without a driver: tunnel acam's normal TCP connection over the cable with `adb forward`
//! (what scrcpy does). On Windows this avoids the Android Open Accessory route entirely, which needs
//! a WinUSB driver bound to the right interface (see TODO.md); all it needs is USB debugging on the
//! phone and the platform-tools `adb`, whose driver Windows already has for most phones.

use anyhow::{Context, Result, bail};
use rust_i18n::t;
use std::path::PathBuf;
use std::process::Command;

/// Makes `127.0.0.1:port` on this computer reach `port` on the phone.
pub fn forward(port: u16) -> Result<()> {
    let adb = find();
    let mut cmd = Command::new(&adb);
    cmd.args(["forward", &format!("tcp:{port}"), &format!("tcp:{port}")]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // No console window flashing up from the GUI on every (re)connect.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = match cmd.output() {
        Ok(out) => out,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => bail!("{}", t!("err.adb_missing")),
        Err(e) => return Err(e).with_context(|| adb.display().to_string()),
    };
    if !out.status.success() {
        let msg = String::from_utf8_lossy(&out.stderr);
        bail!("{}", t!("err.adb_forward", error = msg.trim()));
    }
    Ok(())
}

/// `ACAM_ADB`, else the usual platform-tools locations, else `adb` from `PATH`.
fn find() -> PathBuf {
    let exe = if cfg!(windows) { "adb.exe" } else { "adb" };
    if let Some(p) = std::env::var_os("ACAM_ADB") {
        return p.into();
    }
    let mut dirs: Vec<PathBuf> = ["ANDROID_HOME", "ANDROID_SDK_ROOT"]
        .iter()
        .filter_map(|v| std::env::var_os(v))
        .map(|d| PathBuf::from(d).join("platform-tools"))
        .collect();
    if let Some(d) = std::env::var_os("LOCALAPPDATA") {
        dirs.push(PathBuf::from(d).join(r"Android\Sdk\platform-tools"));
    }
    // Where people unzip the standalone platform-tools download.
    if let Some(home) = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }) {
        dirs.push(PathBuf::from(&home).join("platform-tools"));
        dirs.push(PathBuf::from(home).join("Android/Sdk/platform-tools"));
    }
    dirs.into_iter().map(|d| d.join(exe)).find(|p| p.is_file()).unwrap_or_else(|| exe.into())
}
