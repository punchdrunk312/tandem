//! Windows: how this computer is doing (battery, charging, load, disk, accessories, its
//! network address), locking / sleeping, and the programs open here.
//!
//! The quick numbers are read on demand. Two things are slow and are read by a
//! background thread instead, then cached: the batteries of wireless accessories (asked
//! of Windows' device list through PowerShell every two minutes) and the network card's
//! address (once, then every ten minutes).

use crate::proto::{Accessory, AppRef, DeviceInfo, PowerAction};
use std::os::windows::process::CommandExt;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, Once};
use std::time::Duration;
use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::NetworkManagement::IpHelper::{
    GetAdaptersAddresses, GAA_FLAG_INCLUDE_GATEWAYS, GAA_FLAG_SKIP_ANYCAST,
    GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST, IP_ADAPTER_ADDRESSES_LH,
};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
use windows::Win32::System::Power::{GetSystemPowerStatus, SetSuspendState, SYSTEM_POWER_STATUS};
use windows::Win32::System::Shutdown::LockWorkStation;
use windows::Win32::System::SystemInformation::{
    GetTickCount64, GlobalMemoryStatusEx, MEMORYSTATUSEX,
};
use windows::Win32::System::Threading::{GetCurrentProcessId, GetSystemTimes};
use windows::Win32::UI::WindowsAndMessaging::*;

const NO_WINDOW: u32 = 0x0800_0000;

static SLOW: Mutex<(Vec<Accessory>, Option<String>)> = Mutex::new((Vec::new(), None));
static START: Once = Once::new();
/// The last processor times (idle, kernel, user), for the load since then.
static CPU_PREV: Mutex<Option<(u64, u64, u64)>> = Mutex::new(None);
static CPU_LAST: AtomicU64 = AtomicU64::new(0);

fn ft(f: FILETIME) -> u64 {
    ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64
}

fn cpu_percent() -> u8 {
    let (mut i, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
    if unsafe { GetSystemTimes(Some(&mut i), Some(&mut k), Some(&mut u)) }.is_err() {
        return CPU_LAST.load(Relaxed) as u8;
    }
    let now = (ft(i), ft(k), ft(u));
    let mut prev = CPU_PREV.lock().unwrap_or_else(|e| e.into_inner());
    let pct = match *prev {
        Some((pi, pk, pu)) => {
            // Kernel time includes idle time.
            let idle = now.0.saturating_sub(pi);
            let total = now.1.saturating_sub(pk) + now.2.saturating_sub(pu);
            (100 * total.saturating_sub(idle))
                .checked_div(total)
                .map(|p| p.min(100))
                .unwrap_or_else(|| CPU_LAST.load(Relaxed))
        }
        None => 0,
    };
    *prev = Some(now);
    CPU_LAST.store(pct, Relaxed);
    pct as u8
}

pub fn device_info() -> DeviceInfo {
    START.call_once(|| {
        let _ = std::thread::Builder::new()
            .name("tandem-device".into())
            .spawn(slow_thread);
    });
    let mut d = DeviceInfo::default();
    unsafe {
        let mut p = SYSTEM_POWER_STATUS::default();
        if GetSystemPowerStatus(&mut p).is_ok() {
            // 128 = no battery; 255 = unknown.
            let has_battery = p.BatteryFlag & 128 == 0 && p.BatteryFlag != 255 && p.BatteryLifePercent <= 100;
            if has_battery {
                d.battery = Some(p.BatteryLifePercent);
                d.plugged = p.ACLineStatus == 1;
                d.charging = p.BatteryFlag & 8 != 0 || (d.plugged && p.BatteryLifePercent < 100);
                if !d.plugged && p.BatteryLifeTime != u32::MAX {
                    d.mins_left = Some(p.BatteryLifeTime / 60);
                }
                d.saver = p.SystemStatusFlag == 1;
            } else {
                d.plugged = true;
            }
        }
        let mut m = MEMORYSTATUSEX {
            dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
            ..Default::default()
        };
        if GlobalMemoryStatusEx(&mut m).is_ok() {
            d.mem = m.dwMemoryLoad.min(100) as u8;
        }
        let drive: Vec<u16> = std::env::var("SystemDrive")
            .unwrap_or_else(|_| "C:".into())
            .encode_utf16()
            .chain("\\\0".encode_utf16())
            .collect();
        let (mut free, mut total) = (0u64, 0u64);
        if GetDiskFreeSpaceExW(PCWSTR(drive.as_ptr()), Some(&mut free), Some(&mut total), None).is_ok() {
            d.disk_free_gb = (free >> 30) as u32;
            d.disk_total_gb = (total >> 30) as u32;
        }
        d.uptime_min = (GetTickCount64() / 60_000) as u32;
    }
    d.cpu = cpu_percent();
    if let Ok(s) = SLOW.lock() {
        d.accessories = s.0.clone();
        d.mac = s.1.clone();
    }
    d
}

fn slow_thread() {
    let mut n: u64 = 0;
    loop {
        let acc = accessories();
        let mac = if n.is_multiple_of(5) { mac_address() } else { None };
        if let Ok(mut s) = SLOW.lock() {
            if let Some(a) = acc {
                s.0 = a;
            }
            if mac.is_some() {
                s.1 = mac;
            }
        }
        n += 1;
        std::thread::sleep(Duration::from_secs(120));
    }
}

/// Wireless accessories that tell Windows their battery (Bluetooth headsets, mice,
/// keyboards). Windows keeps it as a device property; PowerShell reads it.
fn accessories() -> Option<Vec<Accessory>> {
    const SCRIPT: &str = "$ErrorActionPreference='SilentlyContinue'; \
        Get-PnpDevice -PresentOnly -Status OK | ForEach-Object { \
          $b = (Get-PnpDeviceProperty -InstanceId $_.InstanceId -KeyName '{104EA319-6EE2-4701-BD47-8DDBF425BBE5} 2').Data; \
          if ($b -ne $null) { \"$($_.FriendlyName)|$b\" } }";
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", SCRIPT])
        .creation_flags(NO_WINDOW)
        .output()
        .ok()?;
    Some(crate::status::parse_accessories(&String::from_utf8_lossy(&out.stdout)))
}

/// The address of the network card that has a way out (a gateway), to wake it later.
fn mac_address() -> Option<String> {
    unsafe {
        let flags = GAA_FLAG_INCLUDE_GATEWAYS | GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
        let mut size: u32 = 16 * 1024;
        let mut buf: Vec<u64> = Vec::new();
        for _ in 0..3 {
            buf = vec![0u64; (size as usize).div_ceil(8)];
            let r = GetAdaptersAddresses(0, flags, None, Some(buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH), &mut size);
            if r == 0 {
                break;
            }
            if r != 111 {
                // anything but "buffer too small"
                return None;
            }
        }
        let mut p = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
        while !p.is_null() {
            let a = &*p;
            if a.OperStatus == IfOperStatusUp
                && a.PhysicalAddressLength == 6
                && !a.FirstGatewayAddress.is_null()
                && a.IfType != 24
            // 24 = loopback
            {
                let m = &a.PhysicalAddress[..6];
                return Some(m.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":"));
            }
            p = a.Next;
        }
        None
    }
}

pub fn power(a: PowerAction) {
    unsafe {
        match a {
            PowerAction::Lock => {
                let _ = LockWorkStation();
            }
            PowerAction::Sleep => {
                let _ = SetSuspendState(false, false, false);
            }
        }
    }
}

/// The programs with a window open here that could move to another computer: one entry
/// per program, browsers left out, at most eight.
pub fn open_apps() -> Vec<AppRef> {
    struct Ctx {
        me: u32,
        found: Vec<AppRef>,
    }
    unsafe extern "system" fn each(h: HWND, lp: LPARAM) -> windows::core::BOOL {
        let ctx = &mut *(lp.0 as *mut Ctx);
        if ctx.found.len() >= 8 || !IsWindowVisible(h).as_bool() {
            return TRUE;
        }
        if GetWindow(h, GW_OWNER).map(|o| !o.0.is_null()).unwrap_or(false) {
            return TRUE;
        }
        let ex = GetWindowLongW(h, GWL_EXSTYLE) as u32;
        if ex & WS_EX_TOOLWINDOW.0 != 0 || GetWindowTextLengthW(h) == 0 {
            return TRUE;
        }
        // Hidden "suspended" windows of store apps.
        let mut cloaked: u32 = 0;
        let _ = DwmGetWindowAttribute(h, DWMWA_CLOAKED, &mut cloaked as *mut u32 as *mut core::ffi::c_void, 4);
        if cloaked != 0 {
            return TRUE;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, Some(&mut pid));
        if pid == ctx.me {
            return TRUE;
        }
        if let Some(app) = crate::win::app_of_window_pub(h) {
            if !app.is_browser() && !ctx.found.iter().any(|a| a.exe.eq_ignore_ascii_case(&app.exe)) {
                ctx.found.push(app);
            }
        }
        TRUE
    }
    let mut ctx = Ctx {
        me: unsafe { GetCurrentProcessId() },
        found: Vec::new(),
    };
    unsafe {
        let _ = EnumWindows(Some(each), LPARAM(&mut ctx as *mut Ctx as isize));
    }
    ctx.found
}
