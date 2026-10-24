//! How each computer is doing: battery, charging, how busy it is, its accessories.
//! Every computer reads its own a few times a minute and sends it to the others, so the
//! bar can show a card per computer and celebrate the moments (plugged in, full, low…).
//!
//! The platform reads the raw numbers; what is worked out from them over time (how long
//! until the battery is full) lives here, so it is tested on every machine.

use crate::proto::{AccKind, Accessory, DeviceInfo};
use std::collections::VecDeque;

/// Works out "full in …" from how fast the battery has been filling. Windows only says
/// how long a battery lasts, not how long it takes to charge.
#[derive(Default)]
pub struct ChargeClock {
    /// (seconds, percent) while charging, oldest first.
    samples: VecDeque<(f64, f64)>,
}

/// Look at the last ten minutes: long enough to be steady, short enough to follow a
/// charger being swapped.
const WINDOW_S: f64 = 600.0;

impl ChargeClock {
    /// A new reading at `t` seconds. Returns minutes until full, once that can be told.
    pub fn update(&mut self, t: f64, percent: Option<u8>, charging: bool) -> Option<u32> {
        let p = match (percent, charging) {
            (Some(p), true) if p < 100 => p as f64,
            _ => {
                self.samples.clear();
                return None;
            }
        };
        // A plug-in after a pause, or the clock going backwards: start over.
        if let Some(&(lt, lp)) = self.samples.back() {
            if t < lt || p + 0.5 < lp {
                self.samples.clear();
            }
        }
        self.samples.push_back((t, p));
        while self.samples.len() > 2 && t - self.samples[0].0 > WINDOW_S {
            self.samples.pop_front();
        }
        let &(t0, p0) = self.samples.front()?;
        let (dt, dp) = (t - t0, p - p0);
        // At least two percent climbed and a minute passed, so the rate means something.
        if dt < 60.0 || dp < 2.0 {
            return None;
        }
        let per_min = dp / (dt / 60.0);
        // Batteries charge about half as fast over the last 20%. Measured below that, the
        // last stretch takes twice as long; measured in it, the rate already shows it.
        let left = if p < 80.0 {
            (80.0 - p) / per_min + 20.0 / (per_min / 2.0)
        } else {
            (100.0 - p) / per_min
        };
        Some(left.round().clamp(1.0, 24.0 * 60.0) as u32)
    }
}

/// Is the change between two readings worth telling the other computers about right
/// away (rather than with the next regular update)?
pub fn worth_sending_now(old: &DeviceInfo, new: &DeviceInfo) -> bool {
    old.battery != new.battery
        || old.plugged != new.plugged
        || old.charging != new.charging
        || old.saver != new.saver
        || old.accessories != new.accessories
}

/// "Name|percent" lines; the same name twice (a headset shows up as several devices)
/// counts once.
pub fn parse_accessories(text: &str) -> Vec<Accessory> {
    let mut v: Vec<Accessory> = Vec::new();
    for line in text.lines() {
        let Some((name, pct)) = line.trim().rsplit_once('|') else {
            continue;
        };
        let (name, Ok(b)) = (name.trim(), pct.trim().parse::<u32>()) else {
            continue;
        };
        if name.is_empty() || b > 100 || v.iter().any(|a| a.name == name) {
            continue;
        }
        v.push(Accessory {
            name: name.chars().take(48).collect(),
            battery: b as u8,
            kind: kind_of(name),
            ..Default::default()
        });
        if v.len() >= 12 {
            break;
        }
    }
    v
}

/// What an accessory is, from its name (Windows and macOS rarely say more).
pub fn kind_of(name: &str) -> AccKind {
    let n = name.to_lowercase();
    let has = |w: &[&str]| w.iter().any(|x| n.contains(x));
    if has(&["airpods pro", "airpods (", "airpods", "buds", "earbud", "wf-", "pixel buds", "beats fit", "powerbeats", "jabra elite", "freebuds", "tws"]) && !has(&["max"]) {
        AccKind::Earbuds
    } else if has(&["headphone", "headset", "airpods max", "wh-", "bose", "beats", "hands-free", "hands free", "jbl tune", "sennheiser", "momentum"]) {
        AccKind::Headphones
    } else if has(&["mouse", "mx master", "mx anywhere", "trackpad", "magic trackpad"]) {
        AccKind::Mouse
    } else if has(&["keyboard", "keys", "mx mechanical"]) {
        AccKind::Keyboard
    } else if has(&["controller", "gamepad", "dualsense", "dualshock", "xbox"]) {
        AccKind::Gamepad
    } else if has(&["speaker", "soundlink", "boom", "charge ", "flip "]) {
        AccKind::Speaker
    } else {
        AccKind::Other
    }
}

/// "73%" (what macOS writes) as a number.
fn pct(v: &serde_json::Value) -> Option<u8> {
    let s = v.as_str()?;
    let n: u32 = s.trim().trim_end_matches('%').trim().parse().ok()?;
    (n <= 100).then_some(n as u8)
}

/// The connected Bluetooth devices with a battery, from
/// `system_profiler SPBluetoothDataType -json` (macOS 12 and later).
pub fn parse_mac_bluetooth(json: &str) -> Vec<Accessory> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let roots = v["SPBluetoothDataType"].as_array().cloned().unwrap_or_default();
    for root in roots {
        let Some(list) = root["device_connected"].as_array() else {
            continue;
        };
        for entry in list {
            let Some(obj) = entry.as_object() else { continue };
            for (name, d) in obj {
                let (left, right, case) = (
                    pct(&d["device_batteryLevelLeft"]),
                    pct(&d["device_batteryLevelRight"]),
                    pct(&d["device_batteryLevelCase"]),
                );
                let main = pct(&d["device_batteryLevelMain"]).or(pct(&d["device_batteryLevel"]));
                let parts: Vec<u8> = [left, right].into_iter().flatten().collect();
                let Some(battery) = parts.iter().copied().min().or(main) else {
                    continue;
                };
                let minor = d["device_minorType"].as_str().unwrap_or("").to_lowercase();
                let mut kind = kind_of(name);
                if kind == AccKind::Other {
                    kind = match minor.as_str() {
                        m if m.contains("headphone") || m.contains("headset") => {
                            if left.is_some() { AccKind::Earbuds } else { AccKind::Headphones }
                        }
                        m if m.contains("mouse") => AccKind::Mouse,
                        m if m.contains("keyboard") => AccKind::Keyboard,
                        m if m.contains("speaker") => AccKind::Speaker,
                        m if m.contains("gamepad") || m.contains("joystick") => AccKind::Gamepad,
                        _ => AccKind::Other,
                    };
                }
                if left.is_some() && right.is_some() && kind == AccKind::Headphones {
                    kind = AccKind::Earbuds;
                }
                out.push(Accessory {
                    name: name.chars().take(48).collect(),
                    battery,
                    kind,
                    left,
                    right,
                    case,
                });
                if out.len() >= 12 {
                    return out;
                }
            }
        }
    }
    out
}

/// The Mac's `pmset -g batt`:
/// `Now drawing from 'AC Power'` / ` -InternalBattery-0 (id=…)\t95%; charging; 0:40 remaining present: true`
pub fn parse_pmset(text: &str) -> DeviceInfo {
    let mut d = DeviceInfo {
        plugged: text.contains("'AC Power'"),
        ..Default::default()
    };
    let Some(line) = text.lines().find(|l| l.contains("InternalBattery")) else {
        // A Mac without a battery.
        d.plugged = true;
        return d;
    };
    let Some(pct) = line.split('%').next().and_then(|a| a.rsplit(|c: char| !c.is_ascii_digit()).next()) else {
        return d;
    };
    d.battery = pct.parse::<u8>().ok().filter(|p| *p <= 100);
    let state = line.split(';').nth(1).map(str::trim).unwrap_or("");
    d.charging = state == "charging" || state == "finishing charge";
    if let Some(rem) = line.split(';').nth(2) {
        let t = rem.split_whitespace().next().unwrap_or("");
        if let Some((h, m)) = t.split_once(':') {
            if let (Ok(h), Ok(m)) = (h.parse::<u32>(), m.parse::<u32>()) {
                let mins = h * 60 + m;
                if d.charging {
                    d.mins_to_full = Some(mins);
                } else if !d.plugged {
                    d.mins_left = Some(mins);
                }
            }
        }
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_in_follows_the_charge_rate() {
        let mut c = ChargeClock::default();
        // Not enough to go on at first.
        assert_eq!(c.update(0.0, Some(40), true), None);
        assert_eq!(c.update(30.0, Some(41), true), None);
        // 1% a minute at 42%: 38 minutes to 80%, then 40 for the slower last 20%.
        assert_eq!(c.update(120.0, Some(42), true), Some(78));
        assert_eq!(c.update(300.0, Some(45), true), Some(75));
        // Nearly full, measured in the slow part: just the rate.
        let mut c = ChargeClock::default();
        c.update(0.0, Some(90), true);
        assert_eq!(c.update(240.0, Some(92), true), Some(16));
    }

    #[test]
    fn unplugging_or_a_full_battery_forgets_the_rate() {
        let mut c = ChargeClock::default();
        c.update(0.0, Some(40), true);
        c.update(200.0, Some(44), true);
        assert_eq!(c.update(260.0, Some(44), false), None);
        // Plugged in again: starts from scratch.
        assert_eq!(c.update(270.0, Some(44), true), None);
        assert_eq!(c.update(300.0, Some(100), true), None);
        // No battery at all (a desktop).
        assert_eq!(c.update(400.0, None, true), None);
    }

    #[test]
    fn accessories_from_windows_device_list() {
        let out = "Galaxy Buds2 Pro|80\r\nGalaxy Buds2 Pro|80\nMX Master 3|12\nbroken line\nOdd|250\n|5\n";
        let a = parse_accessories(out);
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].name, "Galaxy Buds2 Pro");
        assert_eq!((a[1].name.as_str(), a[1].battery), ("MX Master 3", 12));
    }

    #[test]
    fn airpods_on_a_mac_tell_each_part() {
        let json = r#"{"SPBluetoothDataType":[{"controller_properties":{},"device_connected":[
            {"Nikolas's AirPods Pro":{"device_address":"AA:BB","device_batteryLevelCase":"60%","device_batteryLevelLeft":"80%","device_batteryLevelRight":"75%","device_minorType":"Headphones"}},
            {"Magic Mouse":{"device_batteryLevelMain":"41%","device_minorType":"Mouse"}},
            {"Some Speaker":{"device_minorType":"Speaker"}}
        ],"device_not_connected":[{"Old Buds":{"device_batteryLevelLeft":"10%"}}]}]}"#;
        let a = parse_mac_bluetooth(json);
        assert_eq!(a.len(), 2, "only connected ones with a battery");
        assert_eq!(a[0].kind, AccKind::Earbuds);
        assert_eq!((a[0].left, a[0].right, a[0].case, a[0].battery), (Some(80), Some(75), Some(60), 75));
        assert_eq!((a[1].kind, a[1].battery), (AccKind::Mouse, 41));
        assert!(parse_mac_bluetooth("not json").is_empty());
    }

    #[test]
    fn accessory_kinds_from_names() {
        assert_eq!(kind_of("Galaxy Buds2 Pro"), AccKind::Earbuds);
        assert_eq!(kind_of("AirPods Max"), AccKind::Headphones);
        assert_eq!(kind_of("WH-1000XM5 Hands-Free AG"), AccKind::Headphones);
        assert_eq!(kind_of("MX Master 3"), AccKind::Mouse);
        assert_eq!(kind_of("Xbox Wireless Controller"), AccKind::Gamepad);
        assert_eq!(kind_of("Thing"), AccKind::Other);
    }

    #[test]
    fn a_macs_battery() {
        let on_battery = "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=4653155)\t54%; discharging; 4:12 remaining present: true\n";
        let d = parse_pmset(on_battery);
        assert_eq!((d.battery, d.plugged, d.charging, d.mins_left), (Some(54), false, false, Some(252)));
        let charging = "Now drawing from 'AC Power'\n -InternalBattery-0 (id=4653155)\t95%; charging; 0:40 remaining present: true\n";
        let d = parse_pmset(charging);
        assert_eq!((d.battery, d.plugged, d.charging, d.mins_to_full), (Some(95), true, true, Some(40)));
        let full = "Now drawing from 'AC Power'\n -InternalBattery-0 (id=1)\t100%; charged; 0:00 remaining present: true\n";
        let d = parse_pmset(full);
        assert_eq!((d.battery, d.charging), (Some(100), false));
        let desktop = "Now drawing from 'AC Power'\n";
        assert_eq!(parse_pmset(desktop).battery, None);
    }

    #[test]
    fn only_real_changes_go_out_at_once() {
        let a = DeviceInfo { battery: Some(50), cpu: 10, ..Default::default() };
        let mut b = a.clone();
        b.cpu = 80;
        assert!(!worth_sending_now(&a, &b), "a busy CPU waits for the next update");
        b.battery = Some(49);
        assert!(worth_sending_now(&a, &b));
        let mut c = a.clone();
        c.plugged = true;
        assert!(worth_sending_now(&a, &c));
    }
}
