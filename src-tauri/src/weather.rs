//! The weather where you are, for Bob and the bar: Open-Meteo (free, no key).
//!
//! Where: the city typed in settings, if there is one. Otherwise this computer's own
//! location from Windows (Wi-Fi and GPS, not the internet address, so a VPN doesn't move
//! you to another country), and the time zone's city if Windows won't say.
//! Place names are looked up in any language and any capitalisation ("ΠΑΤΡΑ", "patras").

use serde::Serialize;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Serialize)]
pub struct Hour {
    /// "14" (local hour).
    pub hour: String,
    pub temp_c: f64,
    pub code: u32,
    pub is_day: bool,
}

#[derive(Serialize)]
pub struct Weather {
    pub city: String,
    pub temp_c: f64,
    /// WMO weather code (0 clear … 95+ thunderstorm).
    pub code: u32,
    pub is_day: bool,
    pub wind_kmh: f64,
    pub feels_c: f64,
    pub high_c: f64,
    pub low_c: f64,
    /// "06:52", local.
    pub sunrise: String,
    pub sunset: String,
    /// The next few hours.
    pub hours: Vec<Hour>,
    /// "device" (this computer's location), "city" (typed), "timezone" (a guess).
    pub source: String,
}

/// This computer's position and its place name, kept a while: Windows is asked at most
/// every half hour.
type Here = (f64, f64, String, Instant);
static HERE: Mutex<Option<Here>> = Mutex::new(None);

#[tauri::command]
pub async fn weather(place: String, fallback: String, lang: String) -> Result<Weather, String> {
    tauri::async_runtime::spawn_blocking(move || fetch(&place, &fallback, &lang))
        .await
        .map_err(|e| e.to_string())?
}

fn fetch(place: &str, fallback: &str, lang: &str) -> Result<Weather, String> {
    let lang = lang.split(['-', '_']).next().unwrap_or("en").to_ascii_lowercase();
    let (lat, lon, city, source) = if !place.trim().is_empty() {
        let (lat, lon, city) = geocode(place.trim(), &lang)?;
        (lat, lon, city, "city")
    } else if let Some((lat, lon, city)) = device_place(&lang) {
        (lat, lon, city, "device")
    } else {
        let (lat, lon, city) = geocode(fallback.trim(), &lang)?;
        (lat, lon, city, "timezone")
    };
    let agent = crate::media::agent();
    let f: serde_json::Value = agent
        .get("https://api.open-meteo.com/v1/forecast")
        .query("latitude", &format!("{lat:.4}"))
        .query("longitude", &format!("{lon:.4}"))
        .query("current", "temperature_2m,apparent_temperature,weather_code,is_day,wind_speed_10m")
        .query("hourly", "temperature_2m,weather_code,is_day")
        .query("daily", "temperature_2m_max,temperature_2m_min,sunrise,sunset")
        .query("timezone", "auto")
        .query("forecast_days", "2")
        .call()
        .map_err(|e| e.to_string())?
        .into_json()
        .map_err(|e| e.to_string())?;
    let c = &f["current"];
    let num = |v: &serde_json::Value| v.as_f64().unwrap_or(0.0);
    // The next six hours from now (the forecast's own local time).
    let now_hour = c["time"].as_str().unwrap_or("").get(..13).unwrap_or("").to_string();
    let times = f["hourly"]["time"].as_array().cloned().unwrap_or_default();
    let start = times
        .iter()
        .position(|t| t.as_str().is_some_and(|t| t.get(..13) == Some(now_hour.as_str())))
        .map(|i| i + 1)
        .unwrap_or(0);
    let hours = (start..start + 6)
        .filter_map(|i| {
            Some(Hour {
                hour: times.get(i)?.as_str()?.get(11..13)?.to_string(),
                temp_c: num(&f["hourly"]["temperature_2m"][i]),
                code: f["hourly"]["weather_code"][i].as_u64().unwrap_or(0) as u32,
                is_day: f["hourly"]["is_day"][i].as_u64().unwrap_or(1) == 1,
            })
        })
        .collect();
    let clock = |v: &serde_json::Value| v.as_str().and_then(|s| s.get(11..16)).unwrap_or("").to_string();
    let d = &f["daily"];
    Ok(Weather {
        city,
        temp_c: num(&c["temperature_2m"]),
        code: c["weather_code"].as_u64().unwrap_or(0) as u32,
        is_day: c["is_day"].as_u64().unwrap_or(1) == 1,
        wind_kmh: num(&c["wind_speed_10m"]),
        feels_c: num(&c["apparent_temperature"]),
        high_c: num(&d["temperature_2m_max"][0]),
        low_c: num(&d["temperature_2m_min"][0]),
        sunrise: clock(&d["sunrise"][0]),
        sunset: clock(&d["sunset"][0]),
        hours,
        source: source.into(),
    })
}

/// "ΠΑΤΡΑ" → "Πατρα", "new YORK" → "New York": how place names are usually written.
fn title_case(s: &str) -> String {
    s.split(' ')
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().chain(c.flat_map(char::to_lowercase)).collect::<String>(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A place name → its position and its name in the person's language. OpenStreetMap
/// first: it knows towns in every language, with or without accents or capitals, and puts
/// the big ones first ("Patras" is the Greek city, not a Spanish village). Open-Meteo if
/// it can't be reached.
fn geocode(place: &str, lang: &str) -> Result<(f64, f64, String), String> {
    if place.is_empty() {
        return Err("No place to look up".into());
    }
    if let Some(hit) = osm_search(place, lang) {
        return Ok(hit);
    }
    let agent = crate::media::agent();
    let mut tries = vec![place.to_string()];
    let tc = title_case(place);
    if tc != place {
        tries.push(tc);
    }
    for name in &tries {
        let geo: serde_json::Value = agent
            .get("https://geocoding-api.open-meteo.com/v1/search")
            .query("name", name)
            .query("count", "1")
            .query("language", lang)
            .call()
            .map_err(|e| e.to_string())?
            .into_json()
            .map_err(|e| e.to_string())?;
        if let Some(hit) = geo["results"].get(0) {
            return Ok((
                hit["latitude"].as_f64().unwrap_or(0.0),
                hit["longitude"].as_f64().unwrap_or(0.0),
                hit["name"].as_str().unwrap_or(place).to_string(),
            ));
        }
    }
    Err(format!("No place called {place}"))
}

/// Towns and cities only, so "Paris" is never a street called Paris.
fn osm_search(place: &str, lang: &str) -> Option<(f64, f64, String)> {
    let osm: serde_json::Value = osm_agent()
        .get("https://nominatim.openstreetmap.org/search")
        .query("q", place)
        .query("format", "jsonv2")
        .query("limit", "1")
        .query("featureType", "settlement")
        .query("accept-language", lang)
        .call()
        .ok()?
        .into_json()
        .ok()?;
    let hit = osm.get(0)?;
    let coord = |k: &str| hit[k].as_str().and_then(|v| v.parse::<f64>().ok());
    let name = hit["name"].as_str().filter(|n| !n.is_empty()).unwrap_or(place).to_string();
    Some((coord("lat")?, coord("lon")?, name))
}

/// OpenStreetMap asks every program to say who it is.
fn osm_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(8))
        .user_agent(concat!("CubeCursor/", env!("CARGO_PKG_VERSION"), " (https://cubecursor.shop)"))
        .build()
}

/// Where this computer is, from Windows, and the name of that town.
fn device_place(lang: &str) -> Option<(f64, f64, String)> {
    if let Some((lat, lon, city, at)) = HERE.lock().unwrap().clone() {
        if at.elapsed() < Duration::from_secs(1800) {
            return Some((lat, lon, city));
        }
    }
    let (lat, lon) = device_position()?;
    let city = place_name(lat, lon, lang).unwrap_or_else(|| "Your location".into());
    *HERE.lock().unwrap() = Some((lat, lon, city.clone(), Instant::now()));
    Some((lat, lon, city))
}

#[cfg(windows)]
fn device_position() -> Option<(f64, f64)> {
    use windows::Devices::Geolocation::{GeolocationAccessStatus, Geolocator, PositionAccuracy};
    use windows::Foundation::TimeSpan;
    // Asks Windows once; the answer is the person's choice in Settings → Privacy → Location.
    let access = Geolocator::RequestAccessAsync().ok()?.get().ok()?;
    if access != GeolocationAccessStatus::Allowed {
        log::info!("weather: Windows location is off for apps, using the time zone");
        return None;
    }
    let g = Geolocator::new().ok()?;
    let _ = g.SetDesiredAccuracy(PositionAccuracy::Default);
    // A position up to 15 minutes old is fine; wait at most 10 seconds for a new one.
    let pos = g
        .GetGeopositionAsyncWithAgeAndTimeout(
            TimeSpan { Duration: 15 * 60 * 10_000_000 },
            TimeSpan { Duration: 10 * 10_000_000 },
        )
        .ok()?
        .get()
        .ok()?;
    let p = pos.Coordinate().ok()?.Point().ok()?.Position().ok()?;
    Some((p.Latitude, p.Longitude))
}

#[cfg(not(windows))]
fn device_position() -> Option<(f64, f64)> {
    None
}

/// The town at this position, in the person's language.
fn place_name(lat: f64, lon: f64, lang: &str) -> Option<String> {
    let v: serde_json::Value = osm_agent()
        .get("https://nominatim.openstreetmap.org/reverse")
        .query("lat", &format!("{lat:.3}"))
        .query("lon", &format!("{lon:.3}"))
        .query("zoom", "10")
        .query("format", "jsonv2")
        .query("accept-language", lang)
        .call()
        .ok()?
        .into_json()
        .ok()?;
    let a = &v["address"];
    ["city", "town", "village", "municipality", "county"]
        .iter()
        .find_map(|k| a[*k].as_str().filter(|s| !s.is_empty()))
        .or_else(|| v["name"].as_str())
        .map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_written_the_usual_way() {
        assert_eq!(title_case("ΠΑΤΡΑ"), "Πατρα");
        assert_eq!(title_case("new YORK"), "New York");
    }
}
