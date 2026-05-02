use anyhow::Context;
use reqwest::Client;
use serde::Deserialize;

use crate::config::Units;

/// Per-day weather summary in the display's local timezone. The
/// multi-day infobox layouts use a `Vec<DailyWeather>` where index 0
/// is "today" and successive entries are the following days.
#[derive(Debug, Clone, Copy)]
pub struct DailyWeather {
    pub temperature_min: f32,
    pub temperature_max: f32,
    /// WMO 4677 weather code — Open-Meteo's "dominant" code for the day.
    pub weather_code: u32,
}

/// Fetch `days` consecutive daily forecasts starting today. Returns
/// at most `days` entries (the API typically returns exactly `days`,
/// but we don't enforce the count). Errors if the response is malformed
/// or the API rejects the request.
pub async fn forecast(
    client: &Client,
    latitude: f32,
    longitude: f32,
    timezone: &str,
    units: Units,
    days: u32,
) -> anyhow::Result<Vec<DailyWeather>> {
    let (temperature_unit, wind_unit) = match units {
        Units::Metric => ("celsius", "kmh"),
        Units::Imperial => ("fahrenheit", "mph"),
    };
    let url = format!(
        "https://api.open-meteo.com/v1/forecast\
         ?latitude={latitude}&longitude={longitude}\
         &daily=temperature_2m_max,temperature_2m_min,weather_code\
         &forecast_days={days}&timezone={timezone}\
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

    let n = resp
        .daily
        .temperature_2m_max
        .len()
        .min(resp.daily.temperature_2m_min.len())
        .min(resp.daily.weather_code.len());
    anyhow::ensure!(n > 0, "weather response had no daily entries");
    Ok((0..n)
        .map(|i| DailyWeather {
            temperature_min: resp.daily.temperature_2m_min[i],
            temperature_max: resp.daily.temperature_2m_max[i],
            weather_code: resp.daily.weather_code[i],
        })
        .collect())
}
