use chrono::{Duration, TimeZone, Utc};
use ostrom::{Client, ContractId, Error, Resolution};
use serde_json::json;
use wiremock::matchers::{body_string, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn setup() -> (MockServer, Client) {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth2/token"))
        // base64("id:secret")
        .and(header("authorization", "Basic aWQ6c2VjcmV0"))
        .and(body_string("grant_type=client_credentials"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "tok",
            "token_type": "Bearer",
            "expires_in": 3600,
        })))
        .expect(1)
        .mount(&server)
        .await;
    let client = Client::with_urls("id", "secret", server.uri(), server.uri());
    (server, client)
}

#[tokio::test]
async fn contracts_and_token_reuse() {
    let (server, client) = setup().await;
    Mock::given(method("GET"))
        .and(path("/contracts"))
        .and(header("authorization", "Bearer tok"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                {
                    "id": 100523456,
                    "type": "ELECTRICITY",
                    "productCode": "SIMPLY_DYNAMIC",
                    "status": "ACTIVE",
                    "customerFirstName": "Max",
                    "customerLastName": "Mustermann",
                    "startDate": "2024-03-22",
                    "currentMonthlyDepositAmount": 120,
                    "address": {
                        "zip": "22083",
                        "city": "Hamburg",
                        "street": "Mozartstr.",
                        "housenumber": "35"
                    }
                },
                { "id": "200", "type": "GAS", "status": "ACTIVE" }
            ]
        })))
        .mount(&server)
        .await;

    let contracts = client.contracts().await.unwrap();
    assert_eq!(contracts.len(), 2);
    assert_eq!(contracts[0].id, ContractId::from(100523456));
    assert_eq!(contracts[1].id, ContractId::from("200"));

    // Second call must reuse the cached token (token mock expects 1 call).
    let c = client.default_contract().await.unwrap();
    assert_eq!(c.zip(), Some("22083"));
    assert_eq!(c.address.unwrap().house_number.as_deref(), Some("35"));
}

#[tokio::test]
async fn spot_prices() {
    let (server, client) = setup().await;
    Mock::given(method("GET"))
        .and(path("/spot-prices"))
        .and(query_param("startDate", "2025-02-06T05:00:00.000Z"))
        .and(query_param("endDate", "2025-02-06T07:00:00.000Z"))
        .and(query_param("resolution", "HOUR"))
        .and(query_param("zip", "22083"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                {
                    "date": "2025-02-06T05:00:00.000Z",
                    "netMwhPrice": 156.12,
                    "netKwhPrice": 15.62,
                    "grossKwhPrice": 18.58,
                    "netKwhTaxAndLevies": 17.73,
                    "grossKwhTaxAndLevies": 21.1,
                    "netMonthlyOstromBaseFee": 5.05,
                    "grossMonthlyOstromBaseFee": 6,
                    "netMonthlyGridFees": 3.69,
                    "grossMonthlyGridFees": 4.39
                },
                {
                    "date": "2025-02-06T06:00:00.000Z",
                    "grossKwhPrice": 21.44,
                    "grossKwhTaxAndLevies": 21.1
                }
            ]
        })))
        .mount(&server)
        .await;

    let start = Utc.with_ymd_and_hms(2025, 2, 6, 5, 0, 0).unwrap();
    let prices = client
        .spot_prices(
            start,
            start + Duration::hours(2),
            Resolution::Hour,
            Some("22083"),
        )
        .await
        .unwrap();
    assert_eq!(prices.len(), 2);
    assert_eq!(prices[0].date, start);
    assert!((prices[0].total_gross_kwh_price() - 39.68).abs() < 1e-9);
    assert_eq!(prices[1].net_mwh_price, None);
}

#[tokio::test]
async fn consumption_chunked() {
    let (server, client) = setup().await;
    let start = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();

    for (from, to, dates) in [
        (
            "2025-01-01T00:00:00.000Z",
            "2025-01-03T00:00:00.000Z",
            vec![
                "2025-01-01T00:00:00Z",
                "2025-01-02T00:00:00Z",
                "2025-01-03T00:00:00Z",
            ],
        ),
        (
            "2025-01-03T00:00:00.000Z",
            "2025-01-04T00:00:00.000Z",
            vec!["2025-01-03T00:00:00Z"],
        ),
    ] {
        let data: Vec<_> = dates
            .iter()
            .map(|d| json!({ "date": d, "kWh": 1.5 }))
            .collect();
        Mock::given(method("GET"))
            .and(path("/contracts/42/energy-consumption"))
            .and(query_param("startDate", from))
            .and(query_param("endDate", to))
            .and(query_param("resolution", "DAY"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": data })))
            .expect(1)
            .mount(&server)
            .await;
    }

    let data = client
        .energy_consumption_chunked(
            &ContractId::from(42),
            start,
            start + Duration::days(3),
            Resolution::Day,
            Duration::days(2),
        )
        .await
        .unwrap();
    // The overlapping 2025-01-03 entry is only included once.
    assert_eq!(data.len(), 3);
    assert_eq!(data.iter().map(|c| c.kwh).sum::<f64>(), 4.5);
}

#[tokio::test]
async fn api_error() {
    let (server, client) = setup().await;
    Mock::given(method("GET"))
        .and(path("/me"))
        .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
        .mount(&server)
        .await;

    match client.me().await {
        Err(Error::Api { status, body }) => {
            assert_eq!(status.as_u16(), 403);
            assert_eq!(body, "forbidden");
        }
        other => panic!("unexpected result: {other:?}"),
    }
}

#[tokio::test]
async fn me() {
    let (server, client) = setup().await;
    Mock::given(method("GET"))
        .and(path("/me"))
        // Response as returned by the sandbox.
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "email": "dummy-email@ostrom-api.io",
            "firstName": "Max",
            "language": "GERMAN",
            "lastName": "Mustermann"
        })))
        .mount(&server)
        .await;

    let me = client.me().await.unwrap();
    assert_eq!(me.first_name.as_deref(), Some("Max"));
    assert_eq!(me.last_name.as_deref(), Some("Mustermann"));
    assert_eq!(me.language.as_deref(), Some("GERMAN"));
    assert!(me.extra.is_empty());
}
