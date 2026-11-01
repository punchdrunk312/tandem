//! The weather where you are, for Bob and the bar: Open-Meteo (free, no key). The place is a
//! city name (typed in settings, or taken from the time zone, "Europe/Athens" → "Athens"),
//! so nothing about this computer is looked up.

use serde::Serialize;

#[derive(Serialize)]
pub struct Weather {
    pub city: String,
    pub temp_c: f64,
    /// WMO weather code (0 clear … 95+ thunderstorm).
    pub code: u32,
    pub is_day: bool,
    pub wind_kmh: f64,
}

#[tauri::command]
pub async fn weather(place: String) -> Result<Weather, String> {
    tauri::async_runtime::spawn_blocking(move || fetch(&place))
        .await
        .map_err(|e| e.to_string())?
}

fn fetch(place: &str) -> Result<Weather, String> {
    let agent = crate::media::agent();
    let geo: serde_json::Value = agent
        .get("https://geocoding-api.open-meteo.com/v1/search")
        .query("name", place.trim())
        .query("count", "1")
        .call()
        .map_err(|e| e.to_string())?
        .into_json()
        .map_err(|e| e.to_string())?;
    let hit = geo["results"].get(0).ok_or_else(|| format!("No place called {place}"))?;
    let (lat, lon) = (hit["latitude"].as_f64().unwrap_or(0.0), hit["longitude"].as_f64().unwrap_or(0.0));
    let now: serde_json::Value = agent
        .get("https://api.open-meteo.com/v1/forecast")
        .query("latitude", &lat.to_string())
        .query("longitude", &lon.to_string())
        .query("current", "temperature_2m,weather_code,is_day,wind_speed_10m")
        .call()
        .map_err(|e| e.to_string())?
        .into_json()
        .map_err(|e| e.to_string())?;
    let c = &now["current"];
    Ok(Weather {
        city: hit["name"].as_str().unwrap_or(place).to_string(),
        temp_c: c["temperature_2m"].as_f64().unwrap_or(0.0),
        code: c["weather_code"].as_u64().unwrap_or(0) as u32,
        is_day: c["is_day"].as_u64().unwrap_or(1) == 1,
        wind_kmh: c["wind_speed_10m"].as_f64().unwrap_or(0.0),
    })
}
