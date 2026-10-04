# ostrom

Rust client library and command line utility for the API of the German
electricity provider [Ostrom](https://www.ostrom.de)
([API docs](https://docs.ostrom-api.io/reference/introduction)).

Supported endpoints:

| Endpoint                                   | Library                        | CLI                  |
|--------------------------------------------|--------------------------------|----------------------|
| `POST /oauth2/token` (client credentials)  | automatic, cached & refreshed  | automatic            |
| `GET /me`                                  | `Client::me`                   | `ostrom me`          |
| `GET /contracts`                           | `Client::contracts`            | `ostrom contracts`   |
| `GET /spot-prices`                         | `Client::spot_prices`          | `ostrom prices`      |
| `GET /contracts/{id}/energy-consumption`   | `Client::energy_consumption`   | `ostrom consumption` |

## Credentials

Create a client ID and secret in the Ostrom developer portal and export them:

```sh
export OSTROM_CLIENT_ID=...
export OSTROM_CLIENT_SECRET=...
# export OSTROM_SANDBOX=true   # to use the sandbox environment
```

The CLI also reads these from a `.env` file in the current directory, or
takes `--client-id` / `--client-secret` / `--sandbox` flags.

## CLI

```sh
cargo install --path .

ostrom contracts
ostrom prices                                  # today + tomorrow, hourly, incl. taxes for your zip
ostrom prices --from now --to +12h --zip 10115
ostrom consumption                             # last 7 days, hourly
ostrom consumption --from 2025-01-01 --to 2026-01-01 --resolution month
ostrom -f csv consumption --from -30d > usage.csv
ostrom -f json prices | jq '.[0]'
```

Times accept `now`, `today`, `yesterday`, `tomorrow`, `YYYY-MM-DD` (local
midnight), RFC 3339 timestamps, or offsets from now: `-1y`, `-3mo` (months),
`-2w`, `-90d`, `+36h`, `-15m` (minutes).
Long consumption ranges are split into requests of at most `--chunk-days` (30).

Prices are in ct/kWh; "gross" includes VAT. The total is energy price plus
taxes, levies and grid fees, which the API only includes when a zip code is
sent (by default the zip of your contract).

## Library

```toml
[dependencies]
ostrom = { path = "...", default-features = false }   # without the CLI deps
```

```rust
use chrono::{Duration, Utc};
use ostrom::{Client, Environment, Resolution};

let client = Client::new(id, secret, Environment::Production);
let contract = client.default_contract().await?;
let now = Utc::now();

for p in client.spot_prices(now, now + Duration::days(1), Resolution::Hour, contract.zip()).await? {
    println!("{}: {:.2} ct/kWh", p.date, p.total_gross_kwh_price());
}

let usage = client
    .energy_consumption(&contract.id, now - Duration::days(7), now, Resolution::Day)
    .await?;
```

The client is async (`reqwest` + rustls) and works with any tokio runtime.
