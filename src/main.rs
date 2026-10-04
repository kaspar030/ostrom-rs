//! Command line utility for the Ostrom API.

use std::process::ExitCode;

use chrono::{DateTime, Duration, DurationRound, Local, Months, NaiveDate, TimeZone, Utc};
use clap::{Parser, Subcommand, ValueEnum};
use ostrom::{
    Address, Client, Consumption, Contract, ContractId, CostReport, Environment, Resolution,
    SpotPrice,
};
use serde::Serialize;

#[derive(Parser)]
#[command(
    version,
    about = "Fetch data from the Ostrom electricity provider API",
    after_help = "Credentials are read from --client-id/--client-secret, the OSTROM_CLIENT_ID/\
OSTROM_CLIENT_SECRET environment variables, or a .env file in the current directory."
)]
struct Cli {
    /// OAuth2 client ID from the Ostrom developer portal.
    #[arg(long, env = "OSTROM_CLIENT_ID", hide_env_values = true,
          value_parser = non_empty)]
    client_id: String,

    /// OAuth2 client secret from the Ostrom developer portal.
    #[arg(long, env = "OSTROM_CLIENT_SECRET", hide_env_values = true,
          value_parser = non_empty)]
    client_secret: String,

    /// Use the sandbox environment instead of production.
    #[arg(long, env = "OSTROM_SANDBOX", global = true)]
    sandbox: bool,

    /// Output format.
    #[arg(long, short, value_enum, default_value_t = Format::Table, global = true)]
    format: Format,

    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Table,
    Json,
    Csv,
}

#[derive(Clone, Copy, ValueEnum)]
enum Res {
    Hour,
    Day,
    Month,
}

impl From<Res> for Resolution {
    fn from(r: Res) -> Self {
        match r {
            Res::Hour => Resolution::Hour,
            Res::Day => Resolution::Day,
            Res::Month => Resolution::Month,
        }
    }
}

const TIME_HELP: &str = "Accepts `now`, `today`, `yesterday`, `tomorrow`, a date (`2025-02-06`, local \
midnight), an RFC 3339 timestamp, or an offset from now: `-1y`, `-3mo` (months), `-2w`, `-90d`, `+36h`, `-15m` (minutes).";

#[derive(Subcommand)]
enum Command {
    /// Show information about the authenticated user.
    Me,
    /// List contracts.
    Contracts,
    /// Show day-ahead spot prices.
    #[command(after_help = TIME_HELP)]
    Prices {
        /// Start of the time range.
        #[arg(long, default_value = "today", value_parser = parse_time, allow_hyphen_values = true)]
        from: DateTime<Utc>,
        /// End of the time range [default: two days after --from].
        #[arg(long, value_parser = parse_time, allow_hyphen_values = true)]
        to: Option<DateTime<Utc>>,
        #[arg(long, value_enum, default_value_t = Res::Hour)]
        resolution: Res,
        /// Zip code used to include taxes, levies and grid fees
        /// [default: zip of your contract].
        #[arg(long, conflicts_with = "no_zip")]
        zip: Option<String>,
        /// Do not send a zip code (raw exchange prices only, no contract lookup).
        #[arg(long)]
        no_zip: bool,
    },
    /// Show smart meter consumption.
    #[command(after_help = TIME_HELP)]
    Consumption {
        /// Contract ID [default: your only / first active electricity contract].
        #[arg(long)]
        contract: Option<String>,
        /// Start of the time range.
        #[arg(long, default_value = "-7d", value_parser = parse_time, allow_hyphen_values = true)]
        from: DateTime<Utc>,
        /// End of the time range.
        #[arg(long, default_value = "now", value_parser = parse_time, allow_hyphen_values = true)]
        to: DateTime<Utc>,
        #[arg(long, value_enum, default_value_t = Res::Hour)]
        resolution: Res,
        /// Maximum number of days fetched per API request.
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..))]
        chunk_days: u32,
    },
    /// Calculate energy costs: hourly consumption times the hourly price
    /// (gross, incl. taxes and levies). Monthly base and grid fees are not
    /// included.
    #[command(after_help = TIME_HELP)]
    Costs {
        /// Contract ID [default: your only / first active electricity contract].
        #[arg(long)]
        contract: Option<String>,
        /// Zip code for taxes, levies and grid fees [default: zip of the contract].
        #[arg(long)]
        zip: Option<String>,
        /// Start of the time range.
        #[arg(long, default_value = "-7d", value_parser = parse_time, allow_hyphen_values = true)]
        from: DateTime<Utc>,
        /// End of the time range.
        #[arg(long, default_value = "now", value_parser = parse_time, allow_hyphen_values = true)]
        to: DateTime<Utc>,
        /// Sum up the output per hour, day or month (local time).
        #[arg(long, value_enum, default_value_t = Group::Hour)]
        group: Group,
        /// Maximum number of days fetched per API request.
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..))]
        chunk_days: u32,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Group {
    Hour,
    Day,
    Month,
}

#[tokio::main]
async fn main() -> ExitCode {
    // Values already set in the environment take precedence over `.env`.
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            let mut source = e.source();
            while let Some(cause) = source {
                eprintln!("  caused by: {cause}");
                source = cause.source();
            }
            ExitCode::FAILURE
        }
    }
}

fn non_empty(s: &str) -> Result<String, String> {
    if s.trim().is_empty() {
        Err("must not be empty".into())
    } else {
        Ok(s.to_owned())
    }
}

async fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let env = if cli.sandbox {
        Environment::Sandbox
    } else {
        Environment::Production
    };
    let client = Client::new(cli.client_id, cli.client_secret, env);

    match cli.command {
        Command::Me => {
            let me = client.me().await?;
            match cli.format {
                Format::Json => print_json(&me)?,
                Format::Table | Format::Csv => {
                    let opt = |s: &Option<String>| s.clone().unwrap_or_default();
                    println!("name:     {} {}", opt(&me.first_name), opt(&me.last_name));
                    println!("email:    {}", opt(&me.email));
                    println!("language: {}", opt(&me.language));
                    for (k, v) in &me.extra {
                        println!("{k}: {v}");
                    }
                }
            }
        }
        Command::Contracts => print_contracts(&client.contracts().await?, cli.format)?,
        Command::Prices {
            from,
            to,
            resolution,
            zip,
            no_zip,
        } => {
            let to = to.unwrap_or(from + Duration::days(2));
            let zip = match (zip, no_zip) {
                (Some(zip), _) => Some(zip),
                (None, true) => None,
                (None, false) => client.default_contract().await?.zip().map(str::to_owned),
            };
            let prices = client
                .spot_prices(from, to, resolution.into(), zip.as_deref())
                .await?;
            print_prices(&prices, cli.format)?;
        }
        Command::Consumption {
            contract,
            from,
            to,
            resolution,
            chunk_days,
        } => {
            let contract = resolve_contract(&client, contract).await?;
            let (from, to) = (hour_trunc(from)?, hour_trunc(to)?);
            let data = client
                .energy_consumption_chunked(
                    &contract.id,
                    from,
                    to,
                    resolution.into(),
                    Duration::days(chunk_days.into()),
                )
                .await?;
            print_consumption(&data, cli.format)?;
        }
        Command::Costs {
            contract,
            zip,
            from,
            to,
            group,
            chunk_days,
        } => {
            let contract = resolve_contract(&client, contract).await?;
            let zip = zip
                .or_else(|| contract.zip().map(str::to_owned))
                .ok_or("contract has no zip code; pass --zip")?;
            let (from, to) = (hour_trunc(from)?, hour_trunc(to)?);
            let report = client
                .costs(
                    &contract.id,
                    &zip,
                    from,
                    to,
                    Duration::days(chunk_days.into()),
                )
                .await?;
            print_costs(&report, group, cli.format)?;
        }
    }
    Ok(())
}

/// Looks up the given contract, or the default one.
async fn resolve_contract(
    client: &Client,
    id: Option<String>,
) -> Result<Contract, Box<dyn std::error::Error>> {
    let Some(id) = id else {
        return Ok(client.default_contract().await?);
    };
    // The API answers unknown IDs with a bare 400; give a better error.
    let id = ContractId::from(id);
    let contracts = client.contracts().await?;
    let ids: Vec<_> = contracts.iter().map(|c| c.id.to_string()).collect();
    contracts
        .into_iter()
        .find(|c| c.id == id)
        .ok_or_else(|| format!("no contract with ID {id} (available: {})", ids.join(", ")).into())
}

/// Aligns to full hours so relative times like `-7d` give clean slots.
fn hour_trunc(t: DateTime<Utc>) -> Result<DateTime<Utc>, chrono::RoundingError> {
    t.duration_trunc(Duration::hours(1))
}

fn parse_time(s: &str) -> Result<DateTime<Utc>, String> {
    let now = Utc::now();
    let local_midnight = |d: NaiveDate| {
        Local
            .from_local_datetime(&d.and_hms_opt(0, 0, 0).unwrap())
            .earliest()
            .map(|t| t.with_timezone(&Utc))
            .ok_or_else(|| format!("invalid local date: {d}"))
    };
    let today = Local::now().date_naive();

    match s {
        "now" => return Ok(now),
        "today" => return local_midnight(today),
        "yesterday" => return local_midnight(today - Duration::days(1)),
        "tomorrow" => return local_midnight(today + Duration::days(1)),
        _ => {}
    }
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Ok(t.with_timezone(&Utc));
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return local_midnight(d);
    }
    if let Some(rest) = s.strip_prefix(['+', '-']) {
        let sign = if s.starts_with('-') { -1 } else { 1 };
        let (num, unit) = rest.split_at(
            rest.find(|c: char| !c.is_ascii_digit())
                .unwrap_or(rest.len()),
        );
        let n: i64 = num.parse().map_err(|_| format!("invalid offset: {s}"))?;
        let months = |m: i64| {
            let m = Months::new(u32::try_from(m).map_err(|_| format!("offset too large: {s}"))?);
            if sign < 0 {
                now.checked_sub_months(m)
            } else {
                now.checked_add_months(m)
            }
            .ok_or_else(|| format!("offset out of range: {s}"))
        };
        let d = match unit {
            "y" => return months(n * 12),
            "mo" => return months(n),
            "w" => Duration::weeks(n),
            "d" => Duration::days(n),
            "h" => Duration::hours(n),
            "m" => Duration::minutes(n),
            _ => {
                return Err(format!(
                    "invalid offset unit in {s:?} (use y, mo, w, d, h or m)"
                ));
            }
        };
        return Ok(now + d * sign);
    }
    Err(format!("cannot parse time: {s:?}"))
}

fn local(t: DateTime<Utc>) -> String {
    t.with_timezone(&Local)
        .format("%Y-%m-%d %H:%M %Z")
        .to_string()
}

fn print_json<T: Serialize + ?Sized>(v: &T) -> serde_json::Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

fn print_contracts(contracts: &[Contract], format: Format) -> serde_json::Result<()> {
    let opt = |s: &Option<String>| s.clone().unwrap_or_default();
    match format {
        Format::Json => print_json(contracts)?,
        Format::Csv => {
            println!("id,type,product,status,start_date,zip,city");
            for c in contracts {
                let addr = c.address.clone().unwrap_or_default();
                println!(
                    "{},{},{},{},{},{},{}",
                    c.id,
                    opt(&c.kind),
                    opt(&c.product_code),
                    opt(&c.status),
                    c.start_date.map(|d| d.to_string()).unwrap_or_default(),
                    opt(&addr.zip),
                    opt(&addr.city),
                );
            }
        }
        Format::Table => {
            println!(
                "{:<12} {:<12} {:<28} {:<10} {:<10} ADDRESS",
                "ID", "TYPE", "PRODUCT", "STATUS", "START"
            );
            for c in contracts {
                println!(
                    "{:<12} {:<12} {:<28} {:<10} {:<10} {}",
                    c.id.to_string(),
                    opt(&c.kind),
                    opt(&c.product_code),
                    opt(&c.status),
                    c.start_date.map(|d| d.to_string()).unwrap_or_default(),
                    c.address
                        .as_ref()
                        .map(Address::one_line)
                        .unwrap_or_default(),
                );
            }
        }
    }
    Ok(())
}

fn print_prices(prices: &[SpotPrice], format: Format) -> serde_json::Result<()> {
    match format {
        Format::Json => print_json(prices)?,
        Format::Csv => {
            println!(
                "date,gross_kwh_price_ct,gross_kwh_tax_and_levies_ct,total_gross_kwh_price_ct"
            );
            for p in prices {
                println!(
                    "{},{},{},{:.2}",
                    p.date.to_rfc3339(),
                    p.gross_kwh_price,
                    p.gross_kwh_tax_and_levies,
                    p.total_gross_kwh_price()
                );
            }
        }
        Format::Table => {
            println!(
                "{:<22} {:>12} {:>12} {:>12}",
                "TIME", "ENERGY ct", "TAXES ct", "TOTAL ct"
            );
            for p in prices {
                println!(
                    "{:<22} {:>12.2} {:>12.2} {:>12.2}",
                    local(p.date),
                    p.gross_kwh_price,
                    p.gross_kwh_tax_and_levies,
                    p.total_gross_kwh_price()
                );
            }
            let totals: Vec<f64> = prices
                .iter()
                .map(SpotPrice::total_gross_kwh_price)
                .collect();
            if let (Some(min), Some(max)) = (
                totals.iter().copied().reduce(f64::min),
                totals.iter().copied().reduce(f64::max),
            ) {
                let avg = totals.iter().sum::<f64>() / totals.len() as f64;
                // Taxes and levies are only returned when a zip code was sent.
                let taxes = if prices.iter().any(|p| p.gross_kwh_tax_and_levies != 0.0) {
                    "incl. taxes and levies"
                } else {
                    "energy only, no taxes and levies returned"
                };
                println!("\nmin {min:.2} / avg {avg:.2} / max {max:.2} ct/kWh (gross, {taxes})");
            }
        }
    }
    Ok(())
}

fn print_consumption(data: &[Consumption], format: Format) -> serde_json::Result<()> {
    match format {
        Format::Json => print_json(data)?,
        Format::Csv => {
            println!("date,kwh");
            for c in data {
                println!("{},{}", c.date.to_rfc3339(), c.kwh);
            }
        }
        Format::Table => {
            println!("{:<22} {:>10}", "TIME", "kWh");
            for c in data {
                println!("{:<22} {:>10.3}", local(c.date), c.kwh);
            }
            let total: f64 = data.iter().map(|c| c.kwh).sum();
            println!("\ntotal {total:.3} kWh");
        }
    }
    Ok(())
}

/// One output row of `costs`, possibly summed over several hours.
#[derive(Serialize)]
struct CostRow {
    period: String,
    kwh: f64,
    cost_eur: f64,
    /// Consumption-weighted average price.
    price_ct_per_kwh: f64,
}

fn print_costs(report: &CostReport, group: Group, format: Format) -> serde_json::Result<()> {
    let fmt = match group {
        Group::Hour => "%Y-%m-%d %H:%M %Z",
        Group::Day => "%Y-%m-%d",
        Group::Month => "%Y-%m",
    };
    let mut rows: Vec<CostRow> = Vec::new();
    for e in &report.entries {
        let period = e.date.with_timezone(&Local).format(fmt).to_string();
        match rows.last_mut() {
            Some(row) if row.period == period => {
                row.kwh += e.kwh;
                row.cost_eur += e.cost_eur;
            }
            _ => rows.push(CostRow {
                period,
                kwh: e.kwh,
                cost_eur: e.cost_eur,
                price_ct_per_kwh: 0.0,
            }),
        }
    }
    for row in &mut rows {
        if row.kwh > 0.0 {
            row.price_ct_per_kwh = row.cost_eur * 100.0 / row.kwh;
        }
    }

    match format {
        Format::Json => print_json(&rows)?,
        Format::Csv => {
            println!("period,kwh,price_ct_per_kwh,cost_eur");
            for r in &rows {
                println!(
                    "{},{},{},{}",
                    r.period, r.kwh, r.price_ct_per_kwh, r.cost_eur
                );
            }
        }
        Format::Table => {
            println!(
                "{:<22} {:>10} {:>10} {:>10}",
                "PERIOD", "kWh", "ct/kWh", "EUR"
            );
            for r in &rows {
                println!(
                    "{:<22} {:>10.3} {:>10.2} {:>10.2}",
                    r.period, r.kwh, r.price_ct_per_kwh, r.cost_eur
                );
            }
            println!(
                "\ntotal {:.3} kWh, {:.2} EUR, avg {:.2} ct/kWh (excl. monthly base and grid fees)",
                report.total_kwh(),
                report.total_eur(),
                report.average_ct_per_kwh().unwrap_or(0.0)
            );
        }
    }
    if !report.unpriced.is_empty() {
        eprintln!(
            "warning: {} consumption slot(s) ({:.3} kWh) had no price and are not included",
            report.unpriced.len(),
            report.unpriced_kwh()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    #[test]
    fn cli_definition_is_valid() {
        super::Cli::command().debug_assert();
    }

    #[test]
    fn negative_offsets_parse_as_values() {
        let args = ["ostrom", "--client-id", "a", "--client-secret", "b"];
        for extra in [
            &["consumption", "--from", "-90d"][..],
            &["consumption", "--from", "-3mo", "--to", "-1w"],
        ] {
            let argv = args.iter().chain(extra);
            assert!(super::Cli::try_parse_from(argv).is_ok(), "{extra:?}");
        }
    }

    #[test]
    fn parse_offsets() {
        use chrono::{Duration, Months, Utc};
        let close = |a: chrono::DateTime<Utc>, b: chrono::DateTime<Utc>| {
            (a - b).abs() < Duration::seconds(5)
        };
        let now = Utc::now();
        assert!(close(
            super::parse_time("-90d").unwrap(),
            now - Duration::days(90)
        ));
        assert!(close(
            super::parse_time("-2w").unwrap(),
            now - Duration::weeks(2)
        ));
        assert!(close(
            super::parse_time("+36h").unwrap(),
            now + Duration::hours(36)
        ));
        assert!(close(
            super::parse_time("-15m").unwrap(),
            now - Duration::minutes(15)
        ));
        assert!(close(
            super::parse_time("-3mo").unwrap(),
            now.checked_sub_months(Months::new(3)).unwrap()
        ));
        assert!(close(
            super::parse_time("-1y").unwrap(),
            now.checked_sub_months(Months::new(12)).unwrap()
        ));
        assert!(super::parse_time("-3x").is_err());
    }
}
