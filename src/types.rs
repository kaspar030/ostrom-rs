//! Data types returned by the Ostrom API.

use std::fmt;

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Deserializer, Serialize};

/// Time resolution for time series queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Resolution {
    Hour,
    Day,
    Month,
}

impl Resolution {
    /// The value used in the `resolution` query parameter.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hour => "HOUR",
            Self::Day => "DAY",
            Self::Month => "MONTH",
        }
    }
}

impl fmt::Display for Resolution {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Wrapper used by list endpoints: `{ "data": [...] }`.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct DataList<T> {
    pub data: Vec<T>,
}

/// A contract ID.
///
/// The API returns these as numbers, but they are used as path segments, so
/// both numbers and strings are accepted when deserializing.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct ContractId(pub String);

impl<'de> Deserialize<'de> for ContractId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Num(u64),
            Str(String),
        }
        Ok(match Raw::deserialize(deserializer)? {
            Raw::Num(n) => Self(n.to_string()),
            Raw::Str(s) => Self(s),
        })
    }
}

impl fmt::Display for ContractId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for ContractId {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl From<String> for ContractId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<u64> for ContractId {
    fn from(n: u64) -> Self {
        Self(n.to_string())
    }
}

/// Supply address of a contract.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Address {
    #[serde(default)]
    pub zip: Option<String>,
    #[serde(default)]
    pub city: Option<String>,
    #[serde(default)]
    pub street: Option<String>,
    #[serde(
        default,
        rename = "housenumber",
        alias = "houseNumber",
        deserialize_with = "string_or_number"
    )]
    pub house_number: Option<String>,
    /// Any fields not covered above.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl Address {
    /// Single-line representation, e.g. `Mozartstr. 35, 22083 Hamburg`.
    pub fn one_line(&self) -> String {
        let join = |a: &Option<String>, b: &Option<String>| {
            [a.as_deref(), b.as_deref()]
                .into_iter()
                .flatten()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        };
        [
            join(&self.street, &self.house_number),
            join(&self.zip, &self.city),
        ]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
    }
}

fn string_or_number<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(match Option::<serde_json::Value>::deserialize(d)? {
        Some(serde_json::Value::String(s)) => Some(s),
        Some(serde_json::Value::Number(n)) => Some(n.to_string()),
        _ => None,
    })
}

/// An energy supply contract.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Contract {
    pub id: ContractId,
    /// Contract type, e.g. `ENERGY` (sandbox) or `ELECTRICITY`.
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    /// Product code, e.g. `SIMPLY_DYNAMIC`, `SIMPLY_FAIR`.
    #[serde(default)]
    pub product_code: Option<String>,
    /// Contract status, e.g. `ACTIVE`.
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub customer_first_name: Option<String>,
    #[serde(default)]
    pub customer_last_name: Option<String>,
    #[serde(default)]
    pub start_date: Option<NaiveDate>,
    /// Current monthly deposit (Abschlag) in EUR.
    #[serde(default)]
    pub current_monthly_deposit_amount: Option<f64>,
    #[serde(default)]
    pub address: Option<Address>,
    /// Any fields not covered above.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl Contract {
    /// Whether the contract status is `ACTIVE`.
    pub fn is_active(&self) -> bool {
        self.status.as_deref() == Some("ACTIVE")
    }

    /// Whether this is an electricity contract (type `ENERGY` or
    /// `ELECTRICITY`; contracts without a type are assumed to be).
    pub fn is_electricity(&self) -> bool {
        matches!(self.kind.as_deref(), None | Some("ENERGY" | "ELECTRICITY"))
    }

    /// Zip code of the supply address, if known.
    pub fn zip(&self) -> Option<&str> {
        self.address.as_ref()?.zip.as_deref()
    }
}

/// Spot price for one time slot.
///
/// Prices are in **cent**; `*_kwh_*` values are per kWh, `net_mwh_price` is
/// EUR/MWh. "gross" values include VAT.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpotPrice {
    /// Start of the time slot.
    pub date: DateTime<Utc>,
    /// Exchange price in EUR/MWh, without VAT.
    #[serde(default)]
    pub net_mwh_price: Option<f64>,
    /// Energy price in ct/kWh, without VAT.
    #[serde(default)]
    pub net_kwh_price: Option<f64>,
    /// Energy price in ct/kWh, including VAT.
    pub gross_kwh_price: f64,
    /// Taxes, levies and grid charges in ct/kWh, without VAT.
    #[serde(default)]
    pub net_kwh_tax_and_levies: Option<f64>,
    /// Taxes, levies and grid charges in ct/kWh, including VAT.
    pub gross_kwh_tax_and_levies: f64,
    /// Ostrom's monthly base fee in EUR, without VAT.
    #[serde(default)]
    pub net_monthly_ostrom_base_fee: Option<f64>,
    /// Ostrom's monthly base fee in EUR, including VAT.
    #[serde(default)]
    pub gross_monthly_ostrom_base_fee: Option<f64>,
    /// Monthly grid fees in EUR, without VAT.
    #[serde(default)]
    pub net_monthly_grid_fees: Option<f64>,
    /// Monthly grid fees in EUR, including VAT.
    #[serde(default)]
    pub gross_monthly_grid_fees: Option<f64>,
}

impl SpotPrice {
    /// Total price you pay per kWh in ct, including VAT, taxes and levies.
    ///
    /// Only meaningful if the price was requested with a zip code; otherwise
    /// taxes and levies are not included.
    pub fn total_gross_kwh_price(&self) -> f64 {
        self.gross_kwh_price + self.gross_kwh_tax_and_levies
    }
}

/// The authenticated user (`GET /me`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct User {
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub first_name: Option<String>,
    #[serde(default)]
    pub last_name: Option<String>,
    /// Preferred language, e.g. `GERMAN`.
    #[serde(default)]
    pub language: Option<String>,
    /// Any fields not covered above.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Energy consumption for one time slot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Consumption {
    /// Start of the time slot.
    pub date: DateTime<Utc>,
    /// Consumed energy in kWh.
    #[serde(rename = "kWh")]
    pub kwh: f64,
}
