//! Command line utility for the Ostrom API.

use std::process::ExitCode;

use chrono::{DateTime, Duration, Local, NaiveDate, TimeZone, Utc};
use clap::{Parser, Subcommand, ValueEnum};
use ostrom::{Client, Consumption, Contract, ContractId, Environment, Resolution, SpotPrice};
use serde::Serialize;

#[derive(Parser)]
#[command(version, about = "Fetch data from the Ostrom electricity provider API")]
struct Cli {
    /// OAuth2 client ID from the Ostrom developer portal.
    #[arg(long, env = "OSTROM_CLIENT_ID", hide_env_values = true)]
    client_id: String,

    /// OAuth2 client secret from the Ostrom developer portal.
    #[arg(long, env = "OSTROM_CLIENT_SECRET", hide_env_values = true)]
    client_secret: String,

    /// Use the sandbox environment instead of production.
    #[arg(long, env = "OSTROM_SANDBOX")]
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
midnight), an RFC 3339 timestamp, or an offset from now such as `-7d`, `+36h`, `-90m`.";

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
        #[arg(long, default_value = "today", value_parser = parse_time)]
        from: DateTime<Utc>,
        /// End of the time range [default: two days after --from].
        #[arg(long, value_parser = parse_time)]
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
        #[arg(long, default_value = "-7d", value_parser = parse_time)]
        from: DateTime<Utc>,
        /// End of the time range.
        #[arg(long, default_value = "now", value_parser = parse_time)]
        to: DateTime<Utc>,
        #[arg(long, value_enum, default_value_t = Res::Hour)]
        resolution: Res,
        /// Maximum number of days fetched per API request.
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..))]
        chunk_days: u32,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
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
            println!("{}", serde_json::to_string_pretty(&me)?);
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
            let contract = match contract {
                Some(id) => ContractId::from(id),
                None => client.default_contract().await?.id,
            };
            let data = client
                .energy_consumption_chunked(
                    &contract,
                    from,
                    to,
                    resolution.into(),
                    Duration::days(chunk_days.into()),
                )
                .await?;
            print_consumption(&data, cli.format)?;
        }
    }
    Ok(())
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
        let d = match unit {
            "d" => Duration::days(n),
            "h" => Duration::hours(n),
            "m" => Duration::minutes(n),
            _ => return Err(format!("invalid offset unit in {s:?} (use d, h or m)")),
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
                let addr = c.address.clone().unwrap_or_default();
                println!(
                    "{:<12} {:<12} {:<28} {:<10} {:<10} {} {}, {} {}",
                    c.id.to_string(),
                    opt(&c.kind),
                    opt(&c.product_code),
                    opt(&c.status),
                    c.start_date.map(|d| d.to_string()).unwrap_or_default(),
                    opt(&addr.street),
                    opt(&addr.house_number),
                    opt(&addr.zip),
                    opt(&addr.city),
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
                println!(
                    "\nmin {min:.2} / avg {avg:.2} / max {max:.2} ct/kWh (gross, incl. taxes)"
                );
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
