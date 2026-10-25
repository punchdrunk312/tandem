//! Windows: how this computer is doing (battery, charging, load, disk, accessories, its
//! network address), locking / sleeping, and the programs open here.
//!
//! The quick numbers are read on demand. Two things are slow and are read by a
//! background thread instead, then cached: the batteries of wireless accessories (asked
//! of Windows' device list through PowerShell every two minutes) and the network card's
//! address (once, then every ten minutes).

use crate::proto::{AccKind, Accessory, AppRef, DeviceInfo, PowerAction};
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

/// Watches the charger: plugged in or out is noticed within a quarter of a second (asking
/// Windows costs next to nothing), not at the next regular reading a few seconds later.
pub fn power_watch(changed: impl Fn()) {
    let read = || {
        let mut p = SYSTEM_POWER_STATUS::default();
        unsafe { GetSystemPowerStatus(&mut p) }
            .is_ok()
            .then_some((p.ACLineStatus, p.BatteryFlag & 8 != 0))
    };
    let mut last = read();
    loop {
        std::thread::sleep(Duration::from_millis(250));
        let now = read();
        if now != last {
            last = now;
            changed();
        }
    }
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
    loop {
        let mac = mac_address();
        if let Ok(mut s) = SLOW.lock() {
            if mac.is_some() {
                s.1 = mac;
            }
        }
        std::thread::sleep(Duration::from_secs(600));
    }
}

/// Watches the wireless accessories: earbuds connected, a mouse switched on, a battery
/// that went down. Asked of Windows' device tree directly (a few milliseconds), every few
/// seconds, so earbuds show up in the bar about as soon as they connect.
pub fn accessory_watch(changed: impl Fn()) {
    loop {
        if let Some(now) = accessories() {
            let differ = SLOW.lock().map(|s| s.0 != now).unwrap_or(false);
            if differ {
                if let Ok(mut s) = SLOW.lock() {
                    s.0 = now;
                }
                changed();
            }
        }
        std::thread::sleep(Duration::from_secs(4));
    }
}

/// The latest AirPods advert heard nearby: signal strength, (left, right, case), when.
type Pods = (i16, (Option<u8>, Option<u8>, Option<u8>), std::time::Instant);
static APPLE: Mutex<Option<Pods>> = Mutex::new(None);

/// AirPods' charge, from the adverts they send (Windows itself never shows it): the
/// strongest one heard lately, which is the pair in your ears or the case next to you.
fn apple_pods() -> Option<(Option<u8>, Option<u8>, Option<u8>)> {
    let g = APPLE.lock().ok()?;
    let (rssi, p, at) = (*g)?;
    (at.elapsed() < Duration::from_secs(20) && rssi > -75).then_some(p)
}

/// Listens to Bluetooth adverts for AirPods' charge (no pairing, nothing sent).
pub fn pods_watch() {
    use windows::Devices::Bluetooth::Advertisement::*;
    use windows::Foundation::TypedEventHandler;
    use windows::Storage::Streams::DataReader;
    use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
    let run = || -> windows::core::Result<BluetoothLEAdvertisementWatcher> {
        unsafe {
            let _ = RoInitialize(RO_INIT_MULTITHREADED);
        }
        let w = BluetoothLEAdvertisementWatcher::new()?;
        w.SetScanningMode(BluetoothLEScanningMode::Active)?;
        w.Received(&TypedEventHandler::new(
            |_, args: windows::core::Ref<BluetoothLEAdvertisementReceivedEventArgs>| {
                let Some(a) = args.as_ref() else {
                    return Ok(());
                };
                let rssi = a.RawSignalStrengthInDBm()?;
                let list = a.Advertisement()?.ManufacturerData()?;
                for i in 0..list.Size()? {
                    let m = list.GetAt(i)?;
                    if m.CompanyId()? != 0x004C {
                        continue;
                    }
                    let buf = m.Data()?;
                    let mut v = vec![0u8; buf.Length()? as usize];
                    DataReader::FromBuffer(&buf)?.ReadBytes(&mut v)?;
                    let Some(p) = crate::status::parse_apple_pods(&v) else {
                        continue;
                    };
                    if let Ok(mut g) = APPLE.lock() {
                        // Keep the strongest; a weaker one only once that has gone quiet.
                        let take = match *g {
                            Some((r, _, at)) => rssi >= r - 6 || at.elapsed() > Duration::from_secs(6),
                            None => true,
                        };
                        if take {
                            *g = Some((rssi, p, std::time::Instant::now()));
                        }
                    }
                }
                Ok(())
            },
        ))?;
        w.Start()?;
        Ok(w)
    };
    match run() {
        Ok(_watcher) => loop {
            // The watcher lives as long as this thread.
            std::thread::sleep(Duration::from_secs(3600));
        },
        Err(e) => log::info!("airpods: can't listen to Bluetooth adverts ({e})"),
    }
}

/// "AirPods Pro Hands-Free AG" and "AirPods Pro Stereo" are the same AirPods.
fn plain_name(name: &str) -> String {
    let mut n = name.trim().to_string();
    for tail in [" Hands-Free AG Audio", " Hands-Free AG", " Hands-Free", " Stereo", " Avrcp Transport", " LE"] {
        if let Some(cut) = n.strip_suffix(tail) {
            n = cut.trim().to_string();
        }
    }
    n
}

/// Wireless accessories that tell Windows their battery (Bluetooth earbuds, headsets,
/// mice, keyboards): a property Windows keeps on the device, read through CfgMgr32.
fn accessories() -> Option<Vec<Accessory>> {
    use windows::core::GUID;
    use windows::Win32::Devices::DeviceAndDriverInstallation::*;
    use windows::Win32::Devices::Properties::*;
    use windows::Win32::Foundation::DEVPROPKEY;
    const BATTERY: DEVPROPKEY = DEVPROPKEY {
        fmtid: GUID::from_u128(0x104ea319_6ee2_4701_bd47_8ddbf425bbe5),
        pid: 2,
    };
    // Bluetooth devices: connected right now (when Windows says).
    const CONNECTED: DEVPROPKEY = DEVPROPKEY {
        fmtid: GUID::from_u128(0x83da6326_97a6_4088_9453_a1923f573b29),
        pid: 15,
    };
    unsafe {
        let mut len = 0u32;
        if CM_Get_Device_ID_List_SizeW(&mut len, PCWSTR::null(), CM_GETIDLIST_FILTER_PRESENT) != CR_SUCCESS {
            return None;
        }
        let mut ids = vec![0u16; len as usize + 1];
        if CM_Get_Device_ID_ListW(PCWSTR::null(), &mut ids, CM_GETIDLIST_FILTER_PRESENT) != CR_SUCCESS {
            return None;
        }
        let prop = |inst: u32, key: &DEVPROPKEY, buf: &mut [u8]| -> Option<(u32, usize)> {
            let mut ty = DEVPROPTYPE(0);
            let mut size = buf.len() as u32;
            (CM_Get_DevNode_PropertyW(inst, key, &mut ty, Some(buf.as_mut_ptr()), &mut size, 0) == CR_SUCCESS)
                .then_some((ty.0, size as usize))
        };
        let mut out: Vec<Accessory> = Vec::new();
        let mut buf = [0u8; 512];
        for id in ids.split(|&c| c == 0).filter(|s| !s.is_empty()) {
            // Only Bluetooth and HID devices carry the battery.
            let head: String = char::decode_utf16(id.iter().take(8).copied()).filter_map(|c| c.ok()).collect();
            let head = head.to_ascii_uppercase();
            if !(head.starts_with("BTH") || head.starts_with("HID") || head.starts_with("{")) {
                continue;
            }
            let mut z: Vec<u16> = id.to_vec();
            z.push(0);
            let mut inst = 0u32;
            if CM_Locate_DevNodeW(&mut inst, PCWSTR(z.as_ptr()), CM_LOCATE_DEVNODE_NORMAL) != CR_SUCCESS {
                continue;
            }
            // The battery Windows knows (most headsets, mice, keyboards)…
            let battery = match prop(inst, &BATTERY, &mut buf) {
                Some((ty, _)) if ty == DEVPROP_TYPE_BYTE.0 && buf[0] <= 100 => Some(buf[0]),
                _ => None,
            };
            // …or none: AirPods tell theirs only in their adverts (see `apple_pods`).
            if battery.is_none() && !head.starts_with("BTHENUM") {
                continue;
            }
            if let Some((ty, _)) = prop(inst, &CONNECTED, &mut buf) {
                if ty == DEVPROP_TYPE_BOOLEAN.0 && buf[0] == 0 {
                    continue; // paired, but not connected now
                }
            }
            let (mut status, mut problem) = (CM_DEVNODE_STATUS_FLAGS(0), CM_PROB(0));
            if CM_Get_DevNode_Status(&mut status, &mut problem, inst, 0) == CR_SUCCESS && status.0 & DN_STARTED.0 == 0 {
                continue;
            }
            let Some((ty, size)) = prop(inst, &DEVPKEY_Device_FriendlyName, &mut buf) else {
                continue;
            };
            if ty != DEVPROP_TYPE_STRING.0 {
                continue;
            }
            let wide: Vec<u16> = buf[..size.min(buf.len())]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_le_bytes(*c))
                .take_while(|&c| c != 0)
                .collect();
            let name = plain_name(&String::from_utf16_lossy(&wide));
            if name.is_empty() {
                continue;
            }
            let lower = name.to_lowercase();
            let apple = lower.contains("airpods") || lower.contains("beats");
            let pods = if apple { apple_pods() } else { None };
            let (left, right, case) = pods.unwrap_or((None, None, None));
            let battery = match (battery, [left, right].into_iter().flatten().min()) {
                (_, Some(b)) => b,
                (Some(b), None) => b,
                (None, None) => continue,
            };
            match out.iter_mut().find(|a| a.name == name) {
                // The same earbuds show up as several devices: the lowest battery counts.
                Some(a) => {
                    if a.left.is_none() {
                        a.battery = a.battery.min(battery);
                    }
                }
                None => out.push(Accessory {
                    name: name.chars().take(48).collect(),
                    battery,
                    kind: if apple && !lower.contains("max") && !lower.contains("studio") && !lower.contains("solo") {
                        AccKind::Earbuds
                    } else {
                        crate::status::kind_of(&name)
                    },
                    left,
                    right,
                    case,
                }),
            }
            if out.len() >= 12 {
                break;
            }
        }
        Some(out)
    }
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
