//! Matching consumption with spot prices to calculate energy costs.

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

use crate::{Consumption, SpotPrice};

/// Cost of one consumption slot.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CostEntry {
    /// Start of the consumption slot.
    pub date: DateTime<Utc>,
    /// Consumed energy in kWh.
    pub kwh: f64,
    /// Price applied in ct/kWh (gross, incl. taxes and levies).
    pub price_ct_per_kwh: f64,
    /// Cost of this slot in EUR.
    pub cost_eur: f64,
}

/// Result of [`calculate_costs`].
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct CostReport {
    /// Consumption slots for which a price was found.
    pub entries: Vec<CostEntry>,
    /// Consumption slots without a matching price; not included in totals.
    pub unpriced: Vec<Consumption>,
}

impl CostReport {
    /// Total priced consumption in kWh.
    pub fn total_kwh(&self) -> f64 {
        self.entries.iter().map(|e| e.kwh).sum()
    }

    /// Total cost in EUR.
    pub fn total_eur(&self) -> f64 {
        self.entries.iter().map(|e| e.cost_eur).sum()
    }

    /// Consumption-weighted average price in ct/kWh, if anything was consumed.
    pub fn average_ct_per_kwh(&self) -> Option<f64> {
        let kwh = self.total_kwh();
        (kwh > 0.0).then(|| self.total_eur() * 100.0 / kwh)
    }

    /// Consumption without a price, in kWh.
    pub fn unpriced_kwh(&self) -> f64 {
        self.unpriced.iter().map(|c| c.kwh).sum()
    }
}

/// Calculates the energy cost of hourly `consumption` using `prices`.
///
/// Each consumption slot `[t, t + 1h)` is priced with the average total
/// gross price ([`SpotPrice::total_gross_kwh_price`]) of all price slots
/// starting within it (e.g. four 15-minute prices), or, if there are none,
/// with the hourly price slot containing `t`. Slots without a price end up in
/// [`CostReport::unpriced`].
///
/// Prices must have been fetched with a zip code for taxes and levies to be
/// included. Monthly base and grid fees are not included.
pub fn calculate_costs(consumption: &[Consumption], prices: &[SpotPrice]) -> CostReport {
    let slot = Duration::hours(1);
    let mut prices: Vec<&SpotPrice> = prices.iter().collect();
    prices.sort_by_key(|p| p.date);

    let mut report = CostReport::default();
    for c in consumption {
        let first = prices.partition_point(|p| p.date < c.date);
        let within: Vec<f64> = prices[first..]
            .iter()
            .take_while(|p| p.date < c.date + slot)
            .map(|p| p.total_gross_kwh_price())
            .collect();

        let price = if within.is_empty() {
            // Price slot that started before this consumption slot.
            first
                .checked_sub(1)
                .map(|i| prices[i])
                .filter(|p| c.date - p.date < slot)
                .map(SpotPrice::total_gross_kwh_price)
        } else {
            Some(within.iter().sum::<f64>() / within.len() as f64)
        };

        match price {
            Some(price) => report.entries.push(CostEntry {
                date: c.date,
                kwh: c.kwh,
                price_ct_per_kwh: price,
                cost_eur: c.kwh * price / 100.0,
            }),
            None => report.unpriced.push(c.clone()),
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2025, 2, 6, h, m, 0).unwrap()
    }

    fn price(date: DateTime<Utc>, energy: f64, taxes: f64) -> SpotPrice {
        SpotPrice {
            date,
            net_mwh_price: None,
            net_kwh_price: None,
            gross_kwh_price: energy,
            net_kwh_tax_and_levies: None,
            gross_kwh_tax_and_levies: taxes,
            net_monthly_ostrom_base_fee: None,
            gross_monthly_ostrom_base_fee: None,
            net_monthly_grid_fees: None,
            gross_monthly_grid_fees: None,
        }
    }

    fn usage(date: DateTime<Utc>, kwh: f64) -> Consumption {
        Consumption { date, kwh }
    }

    #[test]
    fn hourly_prices() {
        let prices = [price(t(1, 0), 10.0, 20.0), price(t(0, 0), 20.0, 20.0)];
        let consumption = [
            usage(t(0, 0), 1.0),
            usage(t(1, 0), 2.0),
            usage(t(2, 0), 5.0),
        ];
        let r = calculate_costs(&consumption, &prices);

        assert_eq!(r.entries.len(), 2);
        assert_eq!(r.entries[0].price_ct_per_kwh, 40.0);
        assert_eq!(r.entries[1].price_ct_per_kwh, 30.0);
        assert!((r.total_eur() - (0.40 + 0.60)).abs() < 1e-9);
        assert_eq!(r.total_kwh(), 3.0);
        assert!((r.average_ct_per_kwh().unwrap() - 100.0 / 3.0).abs() < 1e-9);
        // No price for 02:00.
        assert_eq!(r.unpriced, vec![usage(t(2, 0), 5.0)]);
        assert_eq!(r.unpriced_kwh(), 5.0);
    }

    #[test]
    fn quarter_hour_prices_are_averaged() {
        let prices: Vec<_> = [10.0, 20.0, 30.0, 40.0]
            .iter()
            .enumerate()
            .map(|(i, &p)| price(t(0, 15 * i as u32), p, 0.0))
            .collect();
        let r = calculate_costs(&[usage(t(0, 0), 2.0)], &prices);
        assert_eq!(r.entries[0].price_ct_per_kwh, 25.0);
        assert!((r.total_eur() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn unaligned_consumption_uses_containing_price_slot() {
        let prices = [price(t(0, 0), 10.0, 0.0), price(t(1, 0), 20.0, 0.0)];
        // Starts within the 01:00 slot; no price starts within [01:30, 02:30)
        let r = calculate_costs(&[usage(t(1, 30), 1.0)], &prices);
        assert_eq!(r.entries[0].price_ct_per_kwh, 20.0);
    }

    #[test]
    fn empty() {
        let r = calculate_costs(&[], &[]);
        assert_eq!(r.total_eur(), 0.0);
        assert_eq!(r.average_ct_per_kwh(), None);
    }
}
