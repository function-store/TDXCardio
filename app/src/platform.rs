//! OS work: find/launch/kill TouchDesigner, focus its window, screenshots, reboot.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};
use sysinfo::{Pid, ProcessesToUpdate, System};

/// Newest TouchDesigner install on this machine.
pub fn find_td_executable() -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();
    #[cfg(windows)]
    {
        for root in ["C:/Program Files/Derivative", "C:/Program Files (x86)/Derivative"] {
            if let Ok(rd) = std::fs::read_dir(root) {
                for e in rd.flatten() {
                    let exe = e.path().join("bin/TouchDesigner.exe");
                    if exe.exists() {
                        found.push(exe);
                    }
                }
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(rd) = std::fs::read_dir("/Applications") {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with("TouchDesigner") && name.ends_with(".app") {
                    let exe = e.path().join("Contents/MacOS/TouchDesigner");
                    if exe.exists() {
                        found.push(exe);
                    }
                }
            }
        }
    }
    // "TouchDesigner.2025.33070" sorts by its numeric parts.
    found.sort_by_key(|p| version_key(p));
    found.pop()
}

fn version_key(p: &Path) -> Vec<u64> {
    p.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .find(|s| s.starts_with("TouchDesigner") && s != "TouchDesigner.exe")
        .map(|s| s.split(|c: char| !c.is_ascii_digit()).filter_map(|n| n.parse().ok()).collect())
        .unwrap_or_default()
}

pub fn launch(exe: &Path, toe: &str, args: &[String]) -> Result<Child, String> {
    let mut cmd = Command::new(exe);
    cmd.arg(toe).args(args);
    if let Some(dir) = Path::new(toe).parent() {
        cmd.current_dir(dir);
    }
    cmd.spawn().map_err(|e| format!("{}: {e}", exe.display()))
}

pub fn pid_alive(pid: u32) -> bool {
    let Ok(mut sys) = System::new() else { return Default::default() };
    let p = Pid::from_u32(pid);
    sys.refresh_processes(ProcessesToUpdate::Some(&[p]), true);
    sys.process(p).is_some()
}

/// Hard kill, then wait (bounded) for the process to be gone.
pub fn kill(pid: u32, wait: Duration) -> bool {
    let Ok(mut sys) = System::new() else { return Default::default() };
    let p = Pid::from_u32(pid);
    sys.refresh_processes(ProcessesToUpdate::Some(&[p]), true);
    if let Some(proc_) = sys.process(p) {
        proc_.kill();
    }
    wait_gone(pid, wait)
}

pub fn wait_gone(pid: u32, wait: Duration) -> bool {
    let until = Instant::now() + wait;
    while Instant::now() < until {
        if !pid_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    !pid_alive(pid)
}

/// Bring the process's main window to the front. No-op when it is already there.
pub fn focus(pid: u32) {
    #[cfg(windows)]
    win::focus(pid);
    #[cfg(target_os = "macos")]
    {
        let script = format!("tell application \"System Events\" to set frontmost of (first process whose unix id is {pid}) to true");
        let _ = Command::new("osascript").args(["-e", &script]).output();
    }
}

pub fn screenshot(dest: &Path) -> Result<(), String> {
    if let Some(d) = dest.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let path = dest.to_string_lossy().replace('\'', "''");
        let ps = format!(
            "Add-Type -AssemblyName System.Windows.Forms,System.Drawing\n\
             $v=[Windows.Forms.SystemInformation]::VirtualScreen\n\
             $b=New-Object Drawing.Bitmap $v.Width,$v.Height\n\
             $g=[Drawing.Graphics]::FromImage($b)\n\
             $g.CopyFromScreen($v.Left,$v.Top,0,0,$b.Size)\n\
             $b.Save('{path}',[Drawing.Imaging.ImageFormat]::Png);$g.Dispose();$b.Dispose()"
        );
        let out = Command::new("powershell")
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &ps])
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
        }
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        let out = Command::new("screencapture").arg("-x").arg(dest).output().map_err(|e| e.to_string())?;
        if out.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&out.stderr).into_owned()) }
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = dest;
        Err("screenshots not supported on this OS".into())
    }
}

pub fn reboot() {
    #[cfg(windows)]
    let _ = Command::new("shutdown").args(["/r", "/t", "30", "/c", "TDXCardio: repeated TouchDesigner failures"]).spawn();
    #[cfg(target_os = "macos")]
    let _ = Command::new("osascript").args(["-e", "tell app \"System Events\" to restart"]).spawn();
}

#[cfg(windows)]
mod win {
    use windows_sys::Win32::Foundation::{HWND, LPARAM, RECT};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{keybd_event, KEYEVENTF_KEYUP, VK_MENU};
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    struct Search {
        pid: u32,
        best: HWND,
        area: i64,
    }

    unsafe extern "system" fn visit(hwnd: HWND, lp: LPARAM) -> i32 {
        unsafe {
            let s = &mut *(lp as *mut Search);
            let mut owner_pid = 0u32;
            GetWindowThreadProcessId(hwnd, &mut owner_pid);
            if owner_pid != s.pid || IsWindowVisible(hwnd) == 0 || !GetWindow(hwnd, GW_OWNER).is_null() {
                return 1;
            }
            let mut r: RECT = std::mem::zeroed();
            GetWindowRect(hwnd, &mut r);
            let area = (r.right - r.left) as i64 * (r.bottom - r.top) as i64;
            if area > s.area {
                s.area = area;
                s.best = hwnd;
            }
            1
        }
    }

    pub fn focus(pid: u32) {
        unsafe {
            let fg = GetForegroundWindow();
            let mut fg_pid = 0u32;
            GetWindowThreadProcessId(fg, &mut fg_pid);
            if fg_pid == pid {
                return; // TD (main window, perform window or its dialog) already has focus
            }
            let mut s = Search { pid, best: std::ptr::null_mut(), area: 0 };
            EnumWindows(Some(visit), &mut s as *mut Search as LPARAM);
            if s.best.is_null() {
                return;
            }
            if IsIconic(s.best) != 0 {
                ShowWindow(s.best, SW_RESTORE);
            }
            // A synthetic Alt tap lets a background process take the foreground.
            keybd_event(VK_MENU as u8, 0, 0, 0);
            keybd_event(VK_MENU as u8, 0, KEYEVENTF_KEYUP, 0);
            SetForegroundWindow(s.best);
        }
    }
}
