use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use regex::Regex;
use serde::Serialize;

use crate::{
    archive::{reserve_archive_page, ArchiveState},
    qlogin::QLoginState,
    qzone,
};

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryProbe {
    label: String,
    offset: u32,
    count: u32,
    begin_time: i64,
    end_time: i64,
    http_status: Option<u16>,
    api_code: Option<i64>,
    subcode: Option<i64>,
    has_more: Option<bool>,
    total_number: Option<i64>,
    record_count: usize,
    oldest_time: Option<i64>,
    newest_time: Option<i64>,
    same_as_baseline: Option<bool>,
    time_upper_bound_matches: Option<bool>,
    note: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryDiagnosticsReport {
    version: u32,
    status: &'static str,
    message: String,
    boundary_offset: u32,
    planned_requests: usize,
    rows: Vec<HistoryProbe>,
}

impl Default for HistoryDiagnosticsReport {
    fn default() -> Self {
        Self {
            version: 1,
            status: "idle",
            message: "尚未运行接口诊断".into(),
            boundary_offset: 10_000,
            planned_requests: 16,
            rows: Vec::new(),
        }
    }
}

#[derive(Default)]
pub struct HistoryDiagnosticsState {
    report: Mutex<HistoryDiagnosticsReport>,
    cancel: AtomicBool,
}

// Strip JS/JSON string values before reading whitelisted scalar metadata.
// A server-provided script is never evaluated. Text inside HTML/messages cannot
// inject a fake code/hasMoreFeeds field into the diagnostic results.
fn response_metadata(body: &str) -> BTreeMap<String, String> {
    let strings = Regex::new(r#"(?s)'(?:\\.|[^'\\])*'|"(?:\\.|[^"\\])*""#).unwrap();
    let masked = strings.replace_all(body, |capture: &regex::Captures<'_>| {
        let value = capture.get(0).unwrap();
        let key = &value.as_str()[1..value.as_str().len() - 1];
        if body[value.end()..].trim_start().starts_with(':')
            && matches!(key, "code" | "subcode" | "hasMoreFeeds" | "total_number")
        {
            value.as_str().to_owned()
        } else {
            "\"\"".into()
        }
    });
    let fields = Regex::new(r#"(?:[,{])\s*["']?(code|subcode|hasMoreFeeds|total_number)["']?\s*:\s*(-?\d+|true|false)\b"#).unwrap();
    let mut result = BTreeMap::new();
    for capture in fields.captures_iter(&masked) {
        result
            .entry(capture[1].to_owned())
            .or_insert_with(|| capture[2].to_owned());
    }
    result
}

// Keys are kept in Rust memory only for comparisons. Neither these identifiers,
// HTML, account IDs, messages, nor authorization values enter the report.
fn sample_records(html: &str) -> Vec<(Option<String>, Option<i64>)> {
    let cards = Regex::new(r"(?is)<li\b(?P<attrs>[^>]*)>(?P<body>.*?)</li>").unwrap();
    let attributes = Regex::new(r#"(?is)\b(class|data-key|id)\s*=\s*["']([^"']*)["']"#).unwrap();
    cards
        .captures_iter(html)
        .filter_map(|card| {
            let fields = attributes
                .captures_iter(&card["attrs"])
                .map(|c| (c[1].to_lowercase(), c[2].to_owned()))
                .collect::<BTreeMap<_, _>>();
            let classes = fields.get("class")?;
            if !["f-single", "f-s-s"]
                .iter()
                .all(|required| classes.split_whitespace().any(|c| c == *required))
            {
                return None;
            }
            let key = fields.get("data-key").or_else(|| fields.get("id")).cloned();
            // Event keys carry Unix seconds. Do not infer a year for an ambiguous
            // on-screen date such as "7月31日", which could fake the time comparison.
            let time = key
                .as_ref()
                .and_then(|key| key.split('_').nth(4))
                .and_then(|value| value.parse::<i64>().ok())
                .filter(|value| (100_000_000..10_000_000_000).contains(value));
            Some((key, time))
        })
        .collect()
}

fn inspect_response(mut row: HistoryProbe, body: &str) -> (HistoryProbe, Option<Vec<String>>) {
    let metadata = response_metadata(body);
    row.api_code = metadata.get("code").and_then(|v| v.parse().ok());
    row.subcode = metadata.get("subcode").and_then(|v| v.parse().ok());
    row.has_more = metadata.get("hasMoreFeeds").and_then(|v| match v.as_str() {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    });
    row.total_number = metadata.get("total_number").and_then(|v| v.parse().ok());
    if row
        .http_status
        .is_some_and(|status| !(200..300).contains(&status))
    {
        row.note = "HTTP 请求失败；未导出响应正文".into();
        return (row, None);
    }
    if row.api_code.is_some_and(|code| code != 0) || row.subcode.is_some_and(|code| code != 0) {
        row.note = if body.to_ascii_lowercase().contains("network busy") {
            "接口返回繁忙"
        } else {
            "接口返回业务错误"
        }
        .into();
        return (row, None);
    }
    match qzone::decode_history_html(body) {
        Ok(html) => {
            let records = sample_records(&html);
            row.record_count = records.len();
            row.oldest_time = records.iter().filter_map(|(_, t)| *t).min();
            row.newest_time = records.iter().filter_map(|(_, t)| *t).max();
            let all_times_known = !records.is_empty() && records.iter().all(|(_, t)| t.is_some());
            if row.end_time > 0 && all_times_known {
                row.time_upper_bound_matches = row.newest_time.map(|time| time <= row.end_time);
            }
            let mut keys = records
                .iter()
                .map(|(key, _)| key.clone())
                .collect::<Option<Vec<_>>>();
            if let Some(keys) = keys.as_mut() {
                keys.sort();
            }
            row.note = if records.is_empty() {
                "已解码 HTML，未识别到历史卡片"
            } else {
                "已取得历史卡片；不写入归档数据库"
            }
            .into();
            (row, keys.filter(|keys| !keys.is_empty()))
        }
        Err(_) => {
            row.note = if row.api_code == Some(0)
                && row.has_more == Some(false)
                && row.total_number == Some(0)
            {
                "成功响应，无更多结果；可能是当前查询结束或接口边界，不能证明历史已全部取回"
            } else {
                "响应缺少可解码的历史 HTML，不能判定为没有数据"
            }
            .into();
            (row, None)
        }
    }
}

fn plan(boundary: u32) -> Vec<HistoryProbe> {
    let mut rows = vec![
        HistoryProbe {
            label: "第一页基准".into(),
            offset: 0,
            count: 30,
            ..Default::default()
        },
        HistoryProbe {
            label: "边界前参考页".into(),
            offset: boundary.saturating_sub(40),
            count: 30,
            ..Default::default()
        },
    ];
    for offset in [
        boundary.saturating_sub(10),
        boundary,
        boundary.saturating_add(20),
        boundary.saturating_add(30),
    ] {
        for count in [1, 10, 30] {
            rows.push(HistoryProbe {
                label: "偏移与窗口对照".into(),
                offset,
                count,
                ..Default::default()
            });
        }
    }
    rows
}

async fn probe(
    login: &QLoginState,
    auth: &crate::qlogin::QzoneAuth,
    mut row: HistoryProbe,
) -> (HistoryProbe, Option<Vec<String>>) {
    let response = qzone::history_messages_request(
        login,
        auth,
        row.offset,
        row.count,
        row.begin_time,
        row.end_time,
    )
    .timeout(Duration::from_secs(20))
    .send()
    .await;
    let mut response = match response {
        Ok(response) => response,
        Err(error) => {
            row.note = if error.is_timeout() {
                "请求超时"
            } else if error.is_connect() {
                "连接失败"
            } else {
                "请求传输失败"
            }
            .into();
            return (row, None);
        }
    };
    row.http_status = Some(response.status().as_u16());
    let mut body = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) if body.len() + chunk.len() <= 4 * 1024 * 1024 => {
                body.extend_from_slice(&chunk)
            }
            Ok(Some(_)) => {
                row.note = "响应超过诊断大小上限，已停止读取".into();
                return (row, None);
            }
            Ok(None) => break,
            Err(_) => {
                row.note = "响应读取失败".into();
                return (row, None);
            }
        }
    }
    inspect_response(row, &String::from_utf8_lossy(&body))
}

#[tauri::command]
pub fn get_history_diagnostics(
    state: tauri::State<'_, HistoryDiagnosticsState>,
) -> Result<HistoryDiagnosticsReport, String> {
    state
        .report
        .lock()
        .map(|report| report.clone())
        .map_err(|_| "诊断状态不可用".into())
}

#[tauri::command]
pub fn cancel_history_diagnostics(state: tauri::State<'_, HistoryDiagnosticsState>) {
    state.cancel.store(true, Ordering::Relaxed);
}

#[tauri::command]
pub async fn start_history_diagnostics(
    app: tauri::AppHandle,
    login: tauri::State<'_, QLoginState>,
    archive: tauri::State<'_, ArchiveState>,
    state: tauri::State<'_, HistoryDiagnosticsState>,
    boundary_offset: u32,
    end_time: Option<i64>,
    interval_ms: u64,
) -> Result<HistoryDiagnosticsReport, String> {
    if boundary_offset > 9_999_970 {
        return Err("边界 offset 必须在 0 到 9999970 之间".into());
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    if end_time.is_some_and(|time| time <= 0 || time > now) {
        return Err("请选择过去的日期时间".into());
    }
    let _gate = archive
        .history_scan_gate
        .try_acquire()
        .map_err(|_| "已有归档或接口诊断正在运行")?;
    let auth = login
        .qzone_auth()
        .await
        .map_err(|_| "登录状态不可用，请在程序中重新登录")?;
    state.cancel.store(false, Ordering::Relaxed);
    *state.report.lock().map_err(|_| "诊断状态不可用")? = HistoryDiagnosticsReport {
        status: "running",
        message: "正在准备诊断；最多 16 次请求".into(),
        boundary_offset,
        ..Default::default()
    };
    let mut requests = plan(boundary_offset);
    let mut baseline_keys: Option<Vec<String>> = None;
    let mut baseline_oldest = None;
    let mut reference_oldest = None;
    let mut failures = 0;
    let mut finish_message = "诊断完成。只反映本次样本，不保证接口的完整可恢复范围。".to_owned();
    let mut time_probes_added = false;
    let delay_ms = interval_ms.clamp(3_000, 30_000);
    let mut index = 0;
    loop {
        if state.cancel.load(Ordering::Relaxed) {
            finish_message = "诊断已停止，已取得的脱敏结果仍可复制".into();
            break;
        }
        if index == requests.len() && !time_probes_added {
            time_probes_added = true;
            let upper = end_time.or(reference_oldest
                .or(baseline_oldest)
                .map(|time: i64| time.saturating_sub(1)));
            if let Some(upper) = upper.filter(|time| *time > 0) {
                for begin_time in [0, 1] {
                    requests.push(HistoryProbe {
                        label: if begin_time == 0 {
                            "仅设置时间上界"
                        } else {
                            "设置时间区间"
                        }
                        .into(),
                        offset: 0,
                        count: 30,
                        begin_time,
                        end_time: upper,
                        ..Default::default()
                    });
                }
            } else {
                finish_message
                    .push_str(" 未取得可靠时间，时间查询测试已跳过；可手动选择上界后重试。");
            }
        }
        let Some(request) = requests.get(index).cloned() else {
            break;
        };
        match reserve_archive_page(&app, &auth.uin) {
            Ok(Some(_)) => {
                finish_message = "已达到程序请求频率保护，诊断提前停止；稍后再试".into();
                break;
            }
            Err(_) => {
                finish_message = "无法记录请求频率，诊断已停止".into();
                break;
            }
            Ok(None) => {}
        }
        {
            let mut report = state.report.lock().map_err(|_| "诊断状态不可用")?;
            report.message = format!(
                "正在诊断第 {} / 16 项：offset {}，窗口 {}",
                index + 1,
                request.offset,
                request.count
            );
        }
        let (mut row, keys) = probe(&login, &auth, request).await;
        if index == 0 {
            baseline_keys = keys.clone();
            baseline_oldest = row.oldest_time;
        }
        if index == 1 {
            reference_oldest = row.oldest_time;
        }
        if row.end_time > 0 {
            row.same_as_baseline = keys
                .as_ref()
                .zip(baseline_keys.as_ref())
                .map(|(keys, baseline)| keys == baseline);
        }
        let failed = row
            .http_status
            .is_none_or(|status| !(200..300).contains(&status))
            || row.api_code.is_some_and(|code| code != 0)
            || row.subcode.is_some_and(|code| code != 0);
        failures = if failed { failures + 1 } else { 0 };
        let limited = row.http_status == Some(429);
        state
            .report
            .lock()
            .map_err(|_| "诊断状态不可用")?
            .rows
            .push(row);
        index += 1;
        if limited || failures >= 3 {
            finish_message = "接口限流或连续三次请求失败，诊断已提前停止，避免反复请求".into();
            break;
        }
        if index < requests.len() || !time_probes_added {
            // Poll the cancellation flag while waiting, without holding a mutex.
            let mut elapsed = 0;
            while elapsed < delay_ms && !state.cancel.load(Ordering::Relaxed) {
                let step = (delay_ms - elapsed).min(200);
                tokio::time::sleep(Duration::from_millis(step)).await;
                elapsed += step;
            }
        }
    }
    let mut report = state.report.lock().map_err(|_| "诊断状态不可用")?;
    report.status = if state.cancel.load(Ordering::Relaxed) {
        "cancelled"
    } else {
        "completed"
    };
    report.message = finish_message;
    Ok(report.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_unquoted_successful_empty_response() {
        let (row, _) = inspect_response(
            HistoryProbe {
                http_status: Some(200),
                ..Default::default()
            },
            r#"_Callback({"code":0,"subcode":0,"data":{main:{hasMoreFeeds:false,total_number:0}}});"#,
        );
        assert_eq!(row.api_code, Some(0));
        assert_eq!(row.has_more, Some(false));
        assert!(row.note.starts_with("成功响应，无更多结果"));
    }

    #[test]
    fn string_content_cannot_inject_metadata_or_enter_report() {
        let body = r#"_Callback({"code":-10001,"subcode":-30,"message":"secret cookie p_skey=private {hasMoreFeeds:false,total_number:0}",html:'private正文',opuin:'123456'});"#;
        let (row, _) = inspect_response(
            HistoryProbe {
                http_status: Some(200),
                ..Default::default()
            },
            body,
        );
        let report = serde_json::to_string(&row).unwrap();
        assert_eq!(row.api_code, Some(-10001));
        assert_eq!(row.has_more, None);
        for secret in ["private", "p_skey", "123456", "正文"] {
            assert!(!report.contains(secret));
        }
    }

    #[test]
    fn extracts_counts_and_event_times_without_exporting_identity() {
        let body = r#"_Callback({code:0,html:'<li class="f-single f-s-s" data-key="fct_987654_217_3_1627745220_1_1"><p>secret body</p></li>',opuin:'123456'});"#;
        let (row, keys) = inspect_response(
            HistoryProbe {
                http_status: Some(200),
                end_time: 1_600_000_000,
                ..Default::default()
            },
            body,
        );
        assert_eq!(row.record_count, 1);
        assert_eq!(row.oldest_time, Some(1_627_745_220));
        assert_eq!(row.time_upper_bound_matches, Some(false));
        assert!(keys.is_some());
        let output = serde_json::to_string(&row).unwrap();
        assert!(!output.contains("987654"));
        assert!(!output.contains("secret body"));
    }

    #[test]
    fn plan_covers_boundary_without_overflow() {
        let requests = plan(10_000);
        assert_eq!(requests.len(), 14);
        for offset in [9990, 10000, 10020, 10030] {
            for count in [1, 10, 30] {
                assert!(requests
                    .iter()
                    .any(|row| row.offset == offset && row.count == count));
            }
        }
        assert!(plan(0).iter().all(|row| row.offset <= 30));
    }
}
