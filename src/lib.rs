//! Client for the [Ostrom](https://www.ostrom.de) electricity provider API.
//!
//! Authentication uses the OAuth2 client credentials flow; create a client ID
//! and secret in the Ostrom developer portal. Access tokens are fetched lazily
//! and refreshed automatically shortly before they expire.
//!
//! ```no_run
//! # async fn run() -> Result<(), ostrom::Error> {
//! use chrono::{Duration, Utc};
//! use ostrom::{Client, Environment, Resolution};
//!
//! let client = Client::new("client-id", "client-secret", Environment::Production);
//!
//! let contract = client.default_contract().await?;
//! let now = Utc::now();
//!
//! let prices = client
//!     .spot_prices(now, now + Duration::days(1), Resolution::Hour, contract.zip())
//!     .await?;
//! for p in prices {
//!     println!("{}: {:.2} ct/kWh", p.date, p.total_gross_kwh_price());
//! }
//!
//! let usage = client
//!     .energy_consumption(&contract.id, now - Duration::days(7), now, Resolution::Day)
//!     .await?;
//! # Ok(())
//! # }
//! ```

mod costs;
mod types;

use std::time::{Duration as StdDuration, Instant};

use chrono::{DateTime, Duration, Utc};
use reqwest::{RequestBuilder, StatusCode};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use tokio::sync::Mutex;

pub use costs::{CostEntry, CostReport, calculate_costs};
pub use types::{Address, Consumption, Contract, ContractId, Resolution, SpotPrice, User};

/// Refresh tokens this long before they actually expire.
const TOKEN_EXPIRY_MARGIN: StdDuration = StdDuration::from_secs(60);

/// Errors returned by this crate.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Transport-level or decoding error.
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    /// The API answered with a non-success status code.
    #[error("API error ({status}): {body}")]
    Api { status: StatusCode, body: String },
    /// No contract matched the request.
    #[error("no matching contract found")]
    NoContract,
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// API environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Environment {
    #[default]
    Production,
    Sandbox,
}

impl Environment {
    /// Base URL of the OAuth2 server.
    pub fn auth_url(self) -> &'static str {
        match self {
            Self::Production => "https://auth.production.ostrom-api.io",
            Self::Sandbox => "https://auth.sandbox.ostrom-api.io",
        }
    }

    /// Base URL of the REST API.
    pub fn api_url(self) -> &'static str {
        match self {
            Self::Production => "https://production.ostrom-api.io",
            Self::Sandbox => "https://sandbox.ostrom-api.io",
        }
    }
}

#[derive(Debug)]
struct Token {
    access_token: String,
    expires_at: Instant,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    expires_in: Option<u64>,
}

/// Ostrom API client.
///
/// Cheap to share behind an `Arc`; all methods take `&self`.
#[derive(Debug)]
pub struct Client {
    http: reqwest::Client,
    client_id: String,
    client_secret: String,
    auth_url: String,
    api_url: String,
    token: Mutex<Option<Token>>,
}

impl Client {
    /// Creates a client for the given environment.
    pub fn new(
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
        environment: Environment,
    ) -> Self {
        Self::with_urls(
            client_id,
            client_secret,
            environment.auth_url(),
            environment.api_url(),
        )
    }

    /// Creates a client with custom base URLs (e.g. for testing).
    pub fn with_urls(
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
        auth_url: impl Into<String>,
        api_url: impl Into<String>,
    ) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(concat!(
                env!("CARGO_PKG_NAME"),
                "/",
                env!("CARGO_PKG_VERSION")
            ))
            .build()
            .expect("failed to build HTTP client");
        Self {
            http,
            client_id: client_id.into(),
            client_secret: client_secret.into(),
            auth_url: auth_url.into().trim_end_matches('/').to_owned(),
            api_url: api_url.into().trim_end_matches('/').to_owned(),
            token: Mutex::new(None),
        }
    }

    /// Returns a valid access token, fetching a new one if needed.
    pub async fn access_token(&self) -> Result<String> {
        let mut token = self.token.lock().await;
        if let Some(t) = token.as_ref()
            && Instant::now() + TOKEN_EXPIRY_MARGIN < t.expires_at
        {
            return Ok(t.access_token.clone());
        }

        let res: TokenResponse = send_json(
            self.http
                .post(format!("{}/oauth2/token", self.auth_url))
                .basic_auth(&self.client_id, Some(&self.client_secret))
                .header(reqwest::header::ACCEPT, "application/json")
                .form(&[("grant_type", "client_credentials")]),
        )
        .await?;

        let expires_in = StdDuration::from_secs(res.expires_in.unwrap_or(3600));
        let access_token = res.access_token;
        *token = Some(Token {
            access_token: access_token.clone(),
            expires_at: Instant::now() + expires_in,
        });
        Ok(access_token)
    }

    async fn get<T: DeserializeOwned>(&self, path: &str, query: &[(&str, String)]) -> Result<T> {
        let token = self.access_token().await?;
        send_json(
            self.http
                .get(format!("{}{}", self.api_url, path))
                .bearer_auth(token)
                .query(query),
        )
        .await
    }

    /// The authenticated user (`GET /me`).
    pub async fn me(&self) -> Result<User> {
        self.get("/me", &[]).await
    }

    /// All contracts of the authenticated user (`GET /contracts`).
    pub async fn contracts(&self) -> Result<Vec<Contract>> {
        let res: types::DataList<Contract> = self.get("/contracts", &[]).await?;
        Ok(res.data)
    }

    /// The contract to use when none is specified explicitly: the only
    /// contract, else the first active electricity contract, else the first
    /// active contract.
    pub async fn default_contract(&self) -> Result<Contract> {
        let mut contracts = self.contracts().await?;
        if contracts.len() == 1 {
            return Ok(contracts.remove(0));
        }
        let pos = contracts
            .iter()
            .position(|c| c.is_active() && c.is_electricity())
            .or_else(|| contracts.iter().position(Contract::is_active))
            .ok_or(Error::NoContract)?;
        Ok(contracts.swap_remove(pos))
    }

    /// Looks up a contract by ID.
    pub async fn contract(&self, id: &ContractId) -> Result<Contract> {
        self.contracts()
            .await?
            .into_iter()
            .find(|c| &c.id == id)
            .ok_or(Error::NoContract)
    }

    /// Day-ahead spot prices (`GET /spot-prices`) for `[start, end)`.
    ///
    /// When `zip` is given, taxes, levies and grid fees for that location are
    /// included in the result.
    pub async fn spot_prices(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        resolution: Resolution,
        zip: Option<&str>,
    ) -> Result<Vec<SpotPrice>> {
        let mut query = vec![
            ("startDate", format_date(start)),
            ("endDate", format_date(end)),
            ("resolution", resolution.as_str().to_owned()),
        ];
        if let Some(zip) = zip {
            query.push(("zip", zip.to_owned()));
        }
        let res: types::DataList<SpotPrice> = self.get("/spot-prices", &query).await?;
        Ok(res.data)
    }

    /// Smart meter consumption of a contract
    /// (`GET /contracts/{id}/energy-consumption`) for `[start, end)`.
    pub async fn energy_consumption(
        &self,
        contract: &ContractId,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        resolution: Resolution,
    ) -> Result<Vec<Consumption>> {
        let query = [
            ("startDate", format_date(start)),
            ("endDate", format_date(end)),
            ("resolution", resolution.as_str().to_owned()),
        ];
        let path = format!("/contracts/{contract}/energy-consumption");
        let res: types::DataList<Consumption> = self.get(&path, &query).await?;
        Ok(res.data)
    }

    /// Like [`Client::energy_consumption`], but splits long ranges into
    /// requests of at most `chunk` each.
    ///
    /// Useful for fetching long hourly histories in one go.
    pub async fn energy_consumption_chunked(
        &self,
        contract: &ContractId,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        resolution: Resolution,
        chunk: Duration,
    ) -> Result<Vec<Consumption>> {
        let mut out: Vec<Consumption> = Vec::new();
        for (from, to) in chunks(start, end, chunk) {
            let data = self
                .energy_consumption(contract, from, to, resolution)
                .await?;
            append_new(&mut out, data, |c| c.date);
        }
        Ok(out)
    }

    /// Like [`Client::spot_prices`], but splits long ranges into requests of
    /// at most `chunk` each.
    pub async fn spot_prices_chunked(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        resolution: Resolution,
        zip: Option<&str>,
        chunk: Duration,
    ) -> Result<Vec<SpotPrice>> {
        let mut out: Vec<SpotPrice> = Vec::new();
        for (from, to) in chunks(start, end, chunk) {
            let data = self.spot_prices(from, to, resolution, zip).await?;
            append_new(&mut out, data, |p| p.date);
        }
        Ok(out)
    }

    /// Energy costs of a contract for `[start, end)`.
    ///
    /// Fetches hourly consumption and spot prices (incl. taxes and levies for
    /// `zip`, usually [`Contract::zip`]) in requests of at most `chunk`, and
    /// matches them with [`calculate_costs`].
    pub async fn costs(
        &self,
        contract: &ContractId,
        zip: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        chunk: Duration,
    ) -> Result<CostReport> {
        let consumption = self
            .energy_consumption_chunked(contract, start, end, Resolution::Hour, chunk)
            .await?;
        let prices = self
            .spot_prices_chunked(start, end, Resolution::Hour, Some(zip), chunk)
            .await?;
        Ok(calculate_costs(&consumption, &prices))
    }
}

/// Splits `[start, end)` into consecutive ranges of at most `chunk`.
fn chunks(
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    chunk: Duration,
) -> impl Iterator<Item = (DateTime<Utc>, DateTime<Utc>)> {
    assert!(chunk > Duration::zero(), "chunk must be positive");
    let mut from = start;
    std::iter::from_fn(move || {
        (from < end).then(|| {
            let to = (from + chunk).min(end);
            (std::mem::replace(&mut from, to), to)
        })
    })
}

/// Appends entries newer than the last one in `out`; chunk boundaries may be
/// inclusive on the server side.
fn append_new<T>(out: &mut Vec<T>, data: Vec<T>, date: impl Fn(&T) -> DateTime<Utc>) {
    for entry in data {
        if out.last().is_none_or(|last| date(&entry) > date(last)) {
            out.push(entry);
        }
    }
}

/// Formats a timestamp as the API expects it (UTC, with milliseconds).
fn format_date(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

async fn send_json<T: DeserializeOwned>(req: RequestBuilder) -> Result<T> {
    let res = req.send().await?;
    let status = res.status();
    if !status.is_success() {
        let body = res.text().await.unwrap_or_default();
        return Err(Error::Api { status, body });
    }
    Ok(res.json().await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn date_format_has_millis() {
        let t = Utc.with_ymd_and_hms(2025, 2, 6, 5, 0, 0).unwrap();
        assert_eq!(format_date(t), "2025-02-06T05:00:00.000Z");
    }
}
