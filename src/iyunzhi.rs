use reqwest::{Client, Method, Url};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;
use time::{Date, Month};

const BASE_URL: &str = "https://lark-biprod.alibaba.com";
const SESSION_RELATIVE_PATH: &str = ".catdesk/iyunzhi_session.json";
const REQUEST_TIMEOUT_SECS: u64 = 30;
const MAX_PAGES: usize = 50;
const DEFAULT_PAGE_SIZE: usize = 200;
const DEFAULT_MAX_ROWS: usize = 200;
const MAX_MAX_ROWS: usize = 1000;
const MAX_RAW_RESPONSE_BYTES: usize = 24_000;

#[derive(Debug, Deserialize)]
struct Session {
    #[serde(rename = "TOKEN")]
    token: String,
    #[serde(rename = "USER_ID")]
    user_id: String,
    #[serde(rename = "LEASE_CODE")]
    lease_code: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TemplateKind {
    CinemaData,
    OperatingStatistic,
    CategoryGroupSale,
    StockTrace,
    GenericDateReport,
}

#[derive(Debug, Clone, Copy)]
struct ReportSpec {
    path: &'static str,
    method: MethodKind,
    template: TemplateKind,
    verified: bool,
}

#[derive(Debug, Clone, Copy)]
enum MethodKind {
    Get,
    Post,
}

impl MethodKind {
    fn reqwest(self) -> Method {
        match self {
            Self::Get => Method::GET,
            Self::Post => Method::POST,
        }
    }
}

#[derive(Debug)]
pub struct Query {
    pub report: String,
    pub begin_date: Option<String>,
    pub end_date: Option<String>,
    pub cinema: Option<String>,
    pub page_size: Option<usize>,
    pub max_rows: Option<usize>,
    pub extra: Map<String, Value>,
    pub contains: BTreeMap<String, String>,
    pub equals: BTreeMap<String, Value>,
}

pub async fn query(workspace_root: &Path, query: Query) -> Result<Value, String> {
    let spec = resolve_report(&query.report).ok_or_else(|| {
        format!(
            "Unsupported BI report '{}'. Use a documented alias/path returned by this tool's schema.",
            query.report
        )
    })?;
    let session = load_session(workspace_root)?;
    let client = Client::builder()
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .build()
        .map_err(|_| "Failed to initialize the BI HTTP client".to_string())?;

    let page_size = query.page_size.unwrap_or(DEFAULT_PAGE_SIZE).clamp(1, 500);
    let max_rows = query
        .max_rows
        .unwrap_or(DEFAULT_MAX_ROWS)
        .clamp(1, MAX_MAX_ROWS);
    let (begin_date, end_date) = normalize_dates(spec, &query)?;

    let cinema_link_ids = if spec.template == TemplateKind::StockTrace {
        let cinema = query.cinema.as_deref().ok_or_else(|| {
            "stock_trace requires a cinema name so cinemaLinkIds can be resolved".to_string()
        })?;
        resolve_cinema_link_ids(&client, &session, cinema).await?
    } else {
        Vec::new()
    };

    let mut all_rows = Vec::new();
    let mut page_no = 1usize;
    let mut first_response: Option<Value> = None;
    let mut reported_total: Option<usize> = None;

    loop {
        let payload = build_payload(
            spec,
            begin_date.as_deref(),
            end_date.as_deref(),
            page_no,
            page_size,
            &cinema_link_ids,
            &query.extra,
        )?;
        let response = send_request(&client, &session, spec, payload).await?;
        ensure_api_success(&response)?;
        if first_response.is_none() {
            first_response = Some(response.clone());
        }

        let Some(rows) = extract_rows(&response) else {
            break;
        };
        if reported_total.is_none() {
            reported_total = extract_total_items(&response);
        }
        let page_count = rows.len();
        all_rows.extend(rows);

        if page_count == 0 || page_no >= MAX_PAGES {
            break;
        }
        if let Some(total) = reported_total {
            if all_rows.len() >= total {
                break;
            }
        } else if page_count < page_size {
            break;
        }
        page_no += 1;
    }

    if !all_rows.is_empty() {
        let before_local_filters = all_rows.len();
        let cinema_filter_applied = query
            .cinema
            .as_deref()
            .is_some_and(|_| all_rows.iter().any(|row| row.get("cinemaName").is_some()));

        if let Some(cinema) = query.cinema.as_deref() {
            if cinema_filter_applied && spec.template != TemplateKind::StockTrace {
                let needle = cinema.to_lowercase();
                all_rows.retain(|row| {
                    row.get("cinemaName")
                        .and_then(Value::as_str)
                        .is_some_and(|name| name.to_lowercase().contains(&needle))
                });
            }
        }
        if !query.contains.is_empty() {
            all_rows.retain(|row| local_contains_match(row, &query.contains));
        }
        if !query.equals.is_empty() {
            all_rows.retain(|row| local_equals_match(row, &query.equals));
        }

        let total_matched = all_rows.len();
        let truncated = total_matched > max_rows;
        all_rows.truncate(max_rows);
        return Ok(json!({
            "success": true,
            "report": query.report,
            "path": spec.path,
            "verifiedTemplate": spec.verified,
            "beginDate": begin_date,
            "endDate": end_date,
            "cinema": query.cinema,
            "cinemaFilterApplied": cinema_filter_applied,
            "rowsBeforeLocalFilters": before_local_filters,
            "totalItems": reported_total,
            "totalMatched": total_matched,
            "truncated": truncated,
            "rows": all_rows,
        }));
    }

    let raw = first_response.unwrap_or_else(|| json!({}));
    Ok(json!({
        "success": true,
        "report": query.report,
        "path": spec.path,
        "verifiedTemplate": spec.verified,
        "beginDate": begin_date,
        "endDate": end_date,
        "cinema": query.cinema,
        "rows": [],
        "raw": bounded_raw(raw),
    }))
}

fn load_session(workspace_root: &Path) -> Result<Session, String> {
    let path = workspace_root.join(SESSION_RELATIVE_PATH);
    let text = std::fs::read_to_string(&path).map_err(|_| {
        format!(
            "Missing local YunZhi session file at {}. Create it on the VPS; credentials are never accepted as tool arguments.",
            path.display()
        )
    })?;
    let session: Session = serde_json::from_str(&text).map_err(|_| {
        "Invalid YunZhi session JSON; expected TOKEN, USER_ID and LEASE_CODE".to_string()
    })?;
    if session.token.trim().is_empty()
        || session.user_id.trim().is_empty()
        || session.lease_code.trim().is_empty()
    {
        return Err("YunZhi session JSON contains an empty credential field".to_string());
    }
    Ok(session)
}

fn normalize_dates(
    spec: ReportSpec,
    query: &Query,
) -> Result<(Option<String>, Option<String>), String> {
    if spec.template == TemplateKind::CinemaData {
        return Ok((query.begin_date.clone(), query.end_date.clone()));
    }
    let begin = query
        .begin_date
        .as_deref()
        .ok_or_else(|| "begin_date is required for BI report queries".to_string())?;
    let end = query.end_date.as_deref().unwrap_or(begin);
    parse_date(begin)?;
    parse_date(end)?;
    Ok((Some(begin.to_string()), Some(end.to_string())))
}

fn build_payload(
    spec: ReportSpec,
    begin: Option<&str>,
    end: Option<&str>,
    page_no: usize,
    page_size: usize,
    cinema_link_ids: &[String],
    extra: &Map<String, Value>,
) -> Result<Option<Value>, String> {
    if matches!(spec.method, MethodKind::Get) {
        return Ok(None);
    }
    let begin = begin.ok_or_else(|| "begin_date is required".to_string())?;
    let end = end.unwrap_or(begin);
    let mut obj = Map::new();
    obj.insert("pageNo".into(), json!(page_no));
    obj.insert("pageSize".into(), json!(page_size));
    obj.insert("beginTime".into(), json!(begin));
    obj.insert("endTime".into(), json!(end));
    obj.insert(
        "gmt".into(),
        json!([
            china_midnight_as_utc(begin)?,
            china_midnight_as_utc(end)?,
            "YYYY-MM-DD"
        ]),
    );
    match spec.template {
        TemplateKind::OperatingStatistic => {
            obj.insert("giftCombo".into(), json!(true));
        }
        TemplateKind::StockTrace => {
            obj.insert("statisticsType".into(), json!("BY_ITEM"));
            obj.insert("cinemaLinkIds".into(), json!(cinema_link_ids));
        }
        _ => {}
    }
    for (key, value) in extra {
        obj.insert(key.clone(), value.clone());
    }
    Ok(Some(Value::Object(obj)))
}

async fn send_request(
    client: &Client,
    session: &Session,
    spec: ReportSpec,
    payload: Option<Value>,
) -> Result<Value, String> {
    let mut url = Url::parse(&format!("{BASE_URL}{}", spec.path))
        .map_err(|_| "Invalid configured BI endpoint".to_string())?;
    url.query_pairs_mut()
        .append_pair("access_token", &session.token);

    let mut request = client
        .request(spec.method.reqwest(), url)
        .header("content-type", "application/json;charset=UTF-8")
        .header("gray-lease-code", &session.lease_code)
        .header("gray-user-id", &session.user_id)
        .header("origin", "https://lark.yuekeyun.com")
        .header("referer", "https://lark.yuekeyun.com/")
        .header("user-agent", "Mozilla/5.0");
    if let Some(payload) = payload {
        request = request.json(&payload);
    }

    let response = request
        .send()
        .await
        .map_err(|_| "YunZhi BI request failed before a response was received".to_string())?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|_| "Failed to read the YunZhi BI response body".to_string())?;
    if !status.is_success() {
        return Err(format!(
            "YunZhi BI returned HTTP {}: {}",
            status.as_u16(),
            safe_body_excerpt(&text)
        ));
    }
    serde_json::from_str(&text).map_err(|_| {
        format!(
            "YunZhi BI returned a non-JSON response: {}",
            safe_body_excerpt(&text)
        )
    })
}

async fn resolve_cinema_link_ids(
    client: &Client,
    session: &Session,
    cinema: &str,
) -> Result<Vec<String>, String> {
    let spec = resolve_report("cinema_data").expect("cinema_data report must exist");
    let response = send_request(client, session, spec, None).await?;
    ensure_api_success(&response)?;
    let rows = extract_rows(&response)
        .ok_or_else(|| "Cinema list response did not contain a row list".to_string())?;
    let needle = cinema.to_lowercase();
    let mut ids = Vec::new();
    for row in rows {
        let name_matches = row
            .get("cinemaName")
            .and_then(Value::as_str)
            .is_some_and(|name| name.to_lowercase().contains(&needle));
        if !name_matches {
            continue;
        }
        if let Some(id) = row.get("cinemaLinkId") {
            if let Some(s) = id.as_str() {
                ids.push(s.to_string());
            } else if id.is_number() {
                ids.push(id.to_string());
            }
        }
    }
    ids.sort();
    ids.dedup();
    if ids.is_empty() {
        return Err(format!("No cinemaLinkId matched cinema name '{cinema}'"));
    }
    Ok(ids)
}

fn ensure_api_success(value: &Value) -> Result<(), String> {
    let Some(code) = value.get("code").and_then(Value::as_str) else {
        return Ok(());
    };
    if code == "SUCCESS" {
        return Ok(());
    }
    let message = value
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("unknown error");
    Err(format!("YunZhi BI API {code}: {message}"))
}

fn extract_rows(value: &Value) -> Option<Vec<Map<String, Value>>> {
    fn find(v: &Value) -> Option<&Vec<Value>> {
        match v {
            Value::Object(obj) => {
                for key in ["list", "items", "records"] {
                    if let Some(Value::Array(rows)) = obj.get(key) {
                        return Some(rows);
                    }
                }
                for child in obj.values() {
                    if let Some(rows) = find(child) {
                        return Some(rows);
                    }
                }
                None
            }
            Value::Array(items) => {
                if items.iter().all(Value::is_object) {
                    return Some(items);
                }
                for child in items {
                    if let Some(rows) = find(child) {
                        return Some(rows);
                    }
                }
                None
            }
            _ => None,
        }
    }
    find(value).map(|rows| rows.iter().filter_map(Value::as_object).cloned().collect())
}

fn extract_total_items(value: &Value) -> Option<usize> {
    fn find(v: &Value) -> Option<u64> {
        match v {
            Value::Object(obj) => {
                for key in ["totalItems", "total", "totalCount"] {
                    if let Some(n) = obj.get(key).and_then(Value::as_u64) {
                        return Some(n);
                    }
                }
                obj.values().find_map(find)
            }
            Value::Array(items) => items.iter().find_map(find),
            _ => None,
        }
    }
    find(value).and_then(|n| usize::try_from(n).ok())
}

fn local_contains_match(row: &Map<String, Value>, filters: &BTreeMap<String, String>) -> bool {
    filters.iter().all(|(field, needle)| {
        row.get(field).is_some_and(|value| {
            let haystack = value
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| value.to_string());
            haystack.to_lowercase().contains(&needle.to_lowercase())
        })
    })
}

fn local_equals_match(row: &Map<String, Value>, filters: &BTreeMap<String, Value>) -> bool {
    filters
        .iter()
        .all(|(field, expected)| row.get(field) == Some(expected))
}

fn bounded_raw(value: Value) -> Value {
    let bytes = serde_json::to_vec(&value)
        .map(|v| v.len())
        .unwrap_or(usize::MAX);
    if bytes <= MAX_RAW_RESPONSE_BYTES {
        return value;
    }
    let keys = value
        .as_object()
        .map(|obj| obj.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    json!({
        "omitted": true,
        "reason": "raw response exceeded the CatDesk BI output budget",
        "topLevelKeys": keys,
        "approxBytes": bytes,
    })
}

fn safe_body_excerpt(text: &str) -> String {
    let compact = text.replace(['\r', '\n'], " ");
    compact.chars().take(800).collect()
}

fn parse_date(input: &str) -> Result<Date, String> {
    let mut parts = input.split('-');
    let year = parts
        .next()
        .and_then(|s| s.parse::<i32>().ok())
        .ok_or_else(|| format!("Invalid date '{input}', expected YYYY-MM-DD"))?;
    let month = parts
        .next()
        .and_then(|s| s.parse::<u8>().ok())
        .and_then(|m| Month::try_from(m).ok())
        .ok_or_else(|| format!("Invalid date '{input}', expected YYYY-MM-DD"))?;
    let day = parts
        .next()
        .and_then(|s| s.parse::<u8>().ok())
        .ok_or_else(|| format!("Invalid date '{input}', expected YYYY-MM-DD"))?;
    if parts.next().is_some() {
        return Err(format!("Invalid date '{input}', expected YYYY-MM-DD"));
    }
    Date::from_calendar_date(year, month, day)
        .map_err(|_| format!("Invalid calendar date '{input}'"))
}

fn china_midnight_as_utc(input: &str) -> Result<String, String> {
    let date = parse_date(input)?;
    let previous = date
        .previous_day()
        .ok_or_else(|| format!("Date '{input}' is outside the supported range"))?;
    Ok(format!("{previous}T16:00:00.000Z"))
}

fn resolve_report(input: &str) -> Option<ReportSpec> {
    let normalized = input.trim();
    let path = match normalized {
        "cinema_data" | "cinemaData" | "/bi/cinema/data" => "/bi/cinema/data",
        "operating_statistic" | "operatingStatistic" | "/bi/marketing/operatingStatistic" => {
            "/bi/marketing/operatingStatistic"
        }
        "category_group_sale" | "categoryGroupSale" | "/bi/goods/categoryGroupSale" => {
            "/bi/goods/categoryGroupSale"
        }
        "stock_trace" | "stockTrace" | "/bi/goods/stockTrace" => "/bi/goods/stockTrace",
        "/bi/ticket/saleDetail" => "/bi/ticket/saleDetail",
        "/bi/ticket/cinemaChannel" => "/bi/ticket/cinemaChannel",
        "/bi/ticket/queryFilmDailySale" => "/bi/ticket/queryFilmDailySale",
        "/bi/ticket/tradeStatisticReport" => "/bi/ticket/tradeStatisticReport",
        "/bi/ticket/tradeSaleRefund" => "/bi/ticket/tradeSaleRefund",
        "/bi/ticket/tradeScheduleRank" => "/bi/ticket/tradeScheduleRank",
        "/bi/ticket/scheduleDetail" => "/bi/ticket/scheduleDetail",
        "/bi/ticket/dayTradeSubaccount" => "/bi/ticket/dayTradeSubaccount",
        "/bi/ticket/monthTradeSubaccount" => "/bi/ticket/monthTradeSubaccount",
        "/bi/marketing/tradeOperatingIncome" => "/bi/marketing/tradeOperatingIncome",
        "/bi/marketing/tradePayment" => "/bi/marketing/tradePayment",
        "/bi/marketing/tradeSaleRank" => "/bi/marketing/tradeSaleRank",
        "/bi/marketing/saleShift" => "/bi/marketing/saleShift",
        "/bi/marketing/ticketActivityFlow" => "/bi/marketing/ticketActivityFlow",
        "/bi/marketing/promoDetails" => "/bi/marketing/promoDetails",
        "/bi/card/rechargeReport" => "/bi/card/rechargeReport",
        "/bi/card/salesReport" => "/bi/card/salesReport",
        "/bi/card/payReport" => "/bi/card/payReport",
        "/bi/card/refundReport" => "/bi/card/refundReport",
        "/bi/card/infoReport" => "/bi/card/infoReport",
        "/bi/card/actReport" => "/bi/card/actReport",
        "/bi/goods/saleSummary" => "/bi/goods/saleSummary",
        "/bi/goods/orderDetail" => "/bi/goods/orderDetail",
        "/bi/goods/profitSummary" => "/bi/goods/profitSummary",
        "/bi/goods/inventory" => "/bi/goods/inventory",
        "/bi/goods/material" => "/bi/goods/material",
        "/bi/coupon/saleReport" => "/bi/coupon/saleReport",
        "/bi/coupon/redeemReport" => "/bi/coupon/redeemReport",
        "/bi/coupon/releaseReport" => "/bi/coupon/releaseReport",
        "/bi/finance/shiftSettlement" => "/bi/finance/shiftSettlement",
        _ => return None,
    };

    let (method, template, verified) = match path {
        "/bi/cinema/data" => (MethodKind::Get, TemplateKind::CinemaData, true),
        "/bi/marketing/operatingStatistic" => {
            (MethodKind::Post, TemplateKind::OperatingStatistic, true)
        }
        "/bi/goods/categoryGroupSale" => (MethodKind::Post, TemplateKind::CategoryGroupSale, true),
        "/bi/goods/stockTrace" => (MethodKind::Post, TemplateKind::StockTrace, true),
        _ => (MethodKind::Post, TemplateKind::GenericDateReport, false),
    };
    Some(ReportSpec {
        path,
        method,
        template,
        verified,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_resolve_to_verified_reports() {
        let operating = resolve_report("operating_statistic").unwrap();
        assert_eq!(operating.path, "/bi/marketing/operatingStatistic");
        assert!(operating.verified);
        assert_eq!(operating.template, TemplateKind::OperatingStatistic);

        let stock = resolve_report("stockTrace").unwrap();
        assert_eq!(stock.path, "/bi/goods/stockTrace");
        assert!(stock.verified);
    }

    #[test]
    fn documented_paths_are_allowed_but_marked_unverified() {
        let report = resolve_report("/bi/card/rechargeReport").unwrap();
        assert_eq!(report.path, "/bi/card/rechargeReport");
        assert!(!report.verified);
        assert_eq!(report.template, TemplateKind::GenericDateReport);
    }

    #[test]
    fn china_midnight_uses_previous_day_at_16_utc() {
        assert_eq!(
            china_midnight_as_utc("2026-09-06").unwrap(),
            "2026-09-05T16:00:00.000Z"
        );
        assert_eq!(
            china_midnight_as_utc("2026-01-01").unwrap(),
            "2025-12-31T16:00:00.000Z"
        );
    }

    #[test]
    fn local_filters_support_contains_and_equals() {
        let row = serde_json::from_value::<Map<String, Value>>(json!({
            "cinemaName": "长沙雨花店",
            "categoryName": "周边衍生品",
            "count": 3
        }))
        .unwrap();
        let contains = BTreeMap::from([("categoryName".to_string(), "衍生".to_string())]);
        let equals = BTreeMap::from([("count".to_string(), json!(3))]);
        assert!(local_contains_match(&row, &contains));
        assert!(local_equals_match(&row, &equals));
    }
}
