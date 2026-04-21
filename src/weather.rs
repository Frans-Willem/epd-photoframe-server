use anyhow::Context;
use reqwest::Client;
use serde::Deserialize;

use crate::config::Units;

/// Daily weather summary for the display's local "today".
#[derive(Debug, Clone, Copy)]
pub struct DailyWeather {
    pub temperature_min: f32,
    pub temperature_max: f32,
    /// WMO 4677 weather code — Open-Meteo's "dominant" code for the day.
    pub weather_code: u32,
}

pub async fn daily(
    client: &Client,
    latitude: f32,
    longitude: f32,
    timezone: &str,
    units: Units,
) -> anyhow::Result<DailyWeather> {
    let (temperature_unit, wind_unit) = match units {
        Units::Metric => ("celsius", "kmh"),
        Units::Imperial => ("fahrenheit", "mph"),
    };
    let url = format!(
        "https://api.open-meteo.com/v1/forecast\
         ?latitude={latitude}&longitude={longitude}\
         &daily=temperature_2m_max,temperature_2m_min,weather_code\
         &forecast_days=1&timezone={timezone}\
         &temperature_unit={temperature_unit}&wind_speed_unit={wind_unit}"
    );
    tracing::debug!(url = %url, "fetching weather");

    #[derive(Deserialize)]
    struct Resp {
        daily: Daily,
    }
    #[derive(Deserialize)]
    struct Daily {
        temperature_2m_max: Vec<f32>,
        temperature_2m_min: Vec<f32>,
        weather_code: Vec<u32>,
    }

    let resp: Resp = client
        .get(&url)
        .send()
        .await
        .context("fetching weather")?
        .error_for_status()
        .context("weather API error")?
        .json()
        .await
        .context("parsing weather JSON")?;

    let max = *resp.daily.temperature_2m_max.first()
        .context("weather response had no max temperature")?;
    let min = *resp.daily.temperature_2m_min.first()
        .context("weather response had no min temperature")?;
    let code = *resp.daily.weather_code.first()
        .context("weather response had no weather code")?;

    Ok(DailyWeather { temperature_min: min, temperature_max: max, weather_code: code })
}
