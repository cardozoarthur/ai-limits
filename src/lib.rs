use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, SecondsFormat, Utc};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const GEMINI_CODE_ASSIST_BASE: &str = "https://cloudcode-pa.googleapis.com/v1internal";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CliProvider {
    Codex,
    Gemini,
}

impl CliProvider {
    fn from_name(name: &str) -> Self {
        if name.eq_ignore_ascii_case("codex") {
            Self::Codex
        } else {
            Self::Gemini
        }
    }

    fn fallback_command(self) -> &'static str {
        match (self, cfg!(windows)) {
            (Self::Codex, true) => "codex.cmd",
            (Self::Codex, false) => "codex",
            (Self::Gemini, true) => "gemini.cmd",
            (Self::Gemini, false) => "gemini",
        }
    }

    fn env_override(self) -> &'static str {
        match self {
            Self::Codex => "AI_LIMITS_CODEX_CMD",
            Self::Gemini => "AI_LIMITS_GEMINI_CMD",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Gemini => "gemini",
        }
    }

    fn display_name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Gemini => "Gemini",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub generated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codex: Option<ProviderStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gemini: Option<ProviderStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderStatus {
    pub name: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cli_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paid_tier: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limits: Vec<ModelLimit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credits: Option<CreditsStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelLimit {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_type: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub windows: Vec<LimitWindowStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credits: Option<CreditsStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LimitWindowStatus {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub used_percent: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining_percent: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining_amount: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_minutes: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub milliseconds_until_reset: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreditsStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_credits: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unlimited: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub balance: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CollectOptions {
    pub timeout: Duration,
    pub filters: ReportFilters,
}

#[derive(Debug, Clone, Default)]
pub struct ReportFilters {
    pub providers: Vec<String>,
    pub models: Vec<String>,
}

impl Default for CollectOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            filters: ReportFilters::default(),
        }
    }
}

pub fn collect_report(options: &CollectOptions) -> Report {
    let now_ms = now_ms();
    let mut report = Report {
        generated_at: iso_from_millis(now_ms),
        codex: provider_is_selected(CliProvider::Codex, &options.filters).then(|| {
            collect_provider(CliProvider::Codex.display_name(), || {
                collect_codex(now_ms, options.timeout)
            })
        }),
        gemini: provider_is_selected(CliProvider::Gemini, &options.filters).then(|| {
            collect_provider(CliProvider::Gemini.display_name(), || {
                collect_gemini(now_ms, options.timeout)
            })
        }),
    };
    apply_report_filters(&mut report, &options.filters);
    report
}

fn collect_provider<F>(name: &str, collect: F) -> ProviderStatus
where
    F: FnOnce() -> Result<ProviderStatus>,
{
    match collect() {
        Ok(status) => status,
        Err(error) => ProviderStatus {
            name: name.to_string(),
            status: "error".to_string(),
            cli_version: read_cli_version(name),
            account: None,
            plan_type: None,
            tier: None,
            paid_tier: None,
            limits: Vec::new(),
            credits: None,
            usage: None,
            source: None,
            error: Some(error.to_string()),
        },
    }
}

fn collect_codex(now_ms: i64, timeout: Duration) -> Result<ProviderStatus> {
    let version = read_cli_version("Codex");
    let (rate_limits, usage) = read_codex_app_server(timeout)?;
    let rate_limits_obj = rate_limits
        .get("rateLimitsByLimitId")
        .and_then(Value::as_object)
        .cloned()
        .or_else(|| {
            rate_limits.get("rateLimits").map(|value| {
                let mut map = serde_json::Map::new();
                let key = value
                    .get("limitId")
                    .and_then(Value::as_str)
                    .unwrap_or("codex")
                    .to_string();
                map.insert(key, value.clone());
                map
            })
        })
        .unwrap_or_default();

    let mut limits = Vec::new();
    for value in rate_limits_obj.values() {
        limits.push(codex_limit_from_value(value, now_ms));
    }
    limits.sort_by(|a, b| a.id.cmp(&b.id));

    let main = rate_limits.get("rateLimits").unwrap_or(&Value::Null);
    let plan_type = main
        .get("planType")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let credits = main.get("credits").and_then(credits_from_value_opt);
    let usage = Some(normalize_codex_usage(&usage));

    Ok(ProviderStatus {
        name: "Codex".to_string(),
        status: "ok".to_string(),
        cli_version: version,
        account: read_codex_account_id(),
        plan_type,
        tier: None,
        paid_tier: None,
        limits,
        credits,
        usage,
        source: Some("codex app-server account/rateLimits/read".to_string()),
        error: None,
    })
}

fn collect_gemini(now_ms: i64, timeout: Duration) -> Result<ProviderStatus> {
    let version = read_cli_version("Gemini");
    let home = home_dir()?;
    let creds_path = home.join(".gemini").join("oauth_creds.json");
    let accounts_path = home.join(".gemini").join("google_accounts.json");
    let creds: GeminiCredentials = read_json_file(&creds_path)
        .with_context(|| format!("failed to read {}", creds_path.display()))?;
    let token = gemini_access_token(&creds, timeout)?;
    let client = Client::builder().timeout(timeout).build()?;
    let account = read_gemini_account(&accounts_path);

    let env_project = env::var("GOOGLE_CLOUD_PROJECT")
        .or_else(|_| env::var("GOOGLE_CLOUD_PROJECT_ID"))
        .ok();
    let mut metadata = json!({
        "ideType": "IDE_UNSPECIFIED",
        "platform": "PLATFORM_UNSPECIFIED",
        "pluginType": "GEMINI"
    });
    if let Some(project) = &env_project {
        metadata["duetProject"] = Value::String(project.clone());
    }
    let mut load_body = json!({ "metadata": metadata });
    if let Some(project) = &env_project {
        load_body["cloudaicompanionProject"] = Value::String(project.clone());
    }

    let load = post_bearer_json(
        &client,
        &format!("{GEMINI_CODE_ASSIST_BASE}:loadCodeAssist"),
        &token,
        &load_body,
    )?;
    let project = load
        .get("cloudaicompanionProject")
        .and_then(Value::as_str)
        .or(env_project.as_deref())
        .ok_or_else(|| anyhow!("Gemini Code Assist did not return a project"))?;
    let quota = post_bearer_json(
        &client,
        &format!("{GEMINI_CODE_ASSIST_BASE}:retrieveUserQuota"),
        &token,
        &json!({ "project": project }),
    )?;

    let mut limits = Vec::new();
    for bucket in quota
        .get("buckets")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        limits.push(gemini_limit_from_bucket(&bucket, now_ms));
    }
    limits.sort_by(|a, b| a.id.cmp(&b.id));

    let tier = load
        .get("currentTier")
        .and_then(|v| v.get("name"))
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let paid_tier = load
        .get("paidTier")
        .and_then(|v| v.get("name"))
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let credits = load
        .get("paidTier")
        .and_then(|v| v.get("availableCredits"))
        .and_then(Value::as_array)
        .map(|credits| gemini_credits_from_available(credits));

    Ok(ProviderStatus {
        name: "Gemini".to_string(),
        status: "ok".to_string(),
        cli_version: version,
        account,
        plan_type: None,
        tier,
        paid_tier,
        limits,
        credits,
        usage: None,
        source: Some("Gemini Code Assist loadCodeAssist/retrieveUserQuota".to_string()),
        error: None,
    })
}

fn read_codex_app_server(timeout: Duration) -> Result<(Value, Value)> {
    let command = resolve_cli_command(CliProvider::Codex);
    let child = spawn_cli_command(&command, &["app-server", "--stdio"])
        .with_context(|| format!("failed to start codex app-server via {}", command.display()))?;
    let mut child = ChildGuard::new(child);

    let mut stdin = child
        .child
        .stdin
        .take()
        .context("failed to open codex stdin")?;
    let stdout = child
        .child
        .stdout
        .take()
        .context("failed to open codex stdout")?;
    let stderr = child.child.stderr.take();

    if let Some(stderr) = stderr {
        thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut discard = String::new();
            while reader.read_line(&mut discard).unwrap_or(0) > 0 {
                discard.clear();
            }
        });
    }

    let (tx, rx) = mpsc::channel::<Value>();
    thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines().map_while(Result::ok) {
            if let Ok(value) = serde_json::from_str::<Value>(&line) {
                let _ = tx.send(value);
            }
        }
    });

    write_json_line(
        &mut stdin,
        &json!({
            "id": 1,
            "method": "initialize",
            "params": {
                "clientInfo": { "name": "ai-limits", "title": null, "version": env!("CARGO_PKG_VERSION") },
                "capabilities": {
                    "experimentalApi": true,
                    "requestAttestation": false,
                    "optOutNotificationMethods": [
                        "thread/started",
                        "thread/status/changed",
                        "remoteControl/status/changed"
                    ]
                }
            }
        }),
    )?;
    wait_for_id(&rx, 1, timeout)?;

    write_json_line(
        &mut stdin,
        &json!({ "id": 2, "method": "account/rateLimits/read" }),
    )?;
    write_json_line(
        &mut stdin,
        &json!({ "id": 3, "method": "account/usage/read" }),
    )?;
    stdin.flush().ok();

    let rate_limits = wait_for_id(&rx, 2, timeout)?;
    let usage = wait_for_id(&rx, 3, timeout)?;
    child.kill_and_wait();

    Ok((
        extract_result(rate_limits).context("codex rate limit response had no result")?,
        extract_result(usage).context("codex usage response had no result")?,
    ))
}

struct ChildGuard {
    child: Child,
}

impl ChildGuard {
    fn new(child: Child) -> Self {
        Self { child }
    }

    fn kill_and_wait(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.kill_and_wait();
    }
}

fn wait_for_id(rx: &mpsc::Receiver<Value>, id: i64, timeout: Duration) -> Result<Value> {
    loop {
        let value = rx
            .recv_timeout(timeout)
            .with_context(|| format!("timed out waiting for codex app-server response id {id}"))?;
        if value.get("id").and_then(Value::as_i64) == Some(id) {
            if let Some(error) = value.get("error") {
                bail!("codex app-server request {id} failed: {error}");
            }
            return Ok(value);
        }
    }
}

fn extract_result(mut value: Value) -> Option<Value> {
    value.as_object_mut()?.remove("result")
}

fn write_json_line(writer: &mut impl Write, value: &Value) -> Result<()> {
    serde_json::to_writer(&mut *writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

#[derive(Debug, Deserialize)]
struct GeminiCredentials {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expiry_date: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct GeminiTokenResponse {
    access_token: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

fn gemini_access_token(creds: &GeminiCredentials, timeout: Duration) -> Result<String> {
    if let (Some(token), Some(expiry)) = (&creds.access_token, creds.expiry_date) {
        if expiry - now_ms() > 60_000 {
            return Ok(token.clone());
        }
    }

    let refresh_token = creds
        .refresh_token
        .as_ref()
        .ok_or_else(|| anyhow!("Gemini OAuth refresh token is missing"))?;
    let oauth_client = gemini_oauth_client()?;
    let client = Client::builder().timeout(timeout).build()?;
    let response = client
        .post("https://oauth2.googleapis.com/token")
        .form(&[
            ("client_id", oauth_client.client_id.as_str()),
            ("client_secret", oauth_client.client_secret.as_str()),
            ("refresh_token", refresh_token),
            ("grant_type", "refresh_token"),
        ])
        .send()
        .context("failed to refresh Gemini OAuth token")?;
    let status = response.status();
    let token_response: GeminiTokenResponse = response
        .json()
        .context("failed to parse Gemini OAuth token response")?;
    if !status.is_success() {
        bail!(
            "Gemini OAuth refresh failed: {} {}",
            token_response.error.unwrap_or_else(|| status.to_string()),
            token_response.error_description.unwrap_or_default()
        );
    }
    token_response
        .access_token
        .ok_or_else(|| anyhow!("Gemini OAuth refresh response did not include access_token"))
}

#[derive(Debug, Clone)]
struct GeminiOAuthClient {
    client_id: String,
    client_secret: String,
}

fn gemini_oauth_client() -> Result<GeminiOAuthClient> {
    if let (Ok(client_id), Ok(client_secret)) = (
        env::var("AI_LIMITS_GEMINI_CLIENT_ID"),
        env::var("AI_LIMITS_GEMINI_CLIENT_SECRET"),
    ) {
        if !client_id.trim().is_empty() && !client_secret.trim().is_empty() {
            return Ok(GeminiOAuthClient {
                client_id,
                client_secret,
            });
        }
    }

    for package_dir in gemini_package_candidates() {
        if let Some(client) = read_gemini_oauth_client_from_package(&package_dir) {
            return Ok(client);
        }
    }

    bail!(
        "could not locate Gemini CLI OAuth client metadata; install Gemini CLI or set AI_LIMITS_GEMINI_CLIENT_ID and AI_LIMITS_GEMINI_CLIENT_SECRET"
    )
}

fn gemini_package_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(value) = env::var_os("AI_LIMITS_GEMINI_CLI_DIR").filter(|value| !value.is_empty()) {
        candidates.push(PathBuf::from(value));
    }
    if let Ok(home) = home_dir() {
        let volta = home.join("AppData").join("Local").join("Volta");
        for node_dir in sorted_volta_node_dirs(&volta) {
            candidates.push(
                node_dir
                    .join("node_modules")
                    .join("@google")
                    .join("gemini-cli"),
            );
        }
    }
    dedupe_paths(candidates)
}

fn read_gemini_oauth_client_from_package(package_dir: &Path) -> Option<GeminiOAuthClient> {
    let bundle_dir = package_dir.join("bundle");
    let entries = fs::read_dir(bundle_dir).ok()?;
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("js") {
            continue;
        }
        let content = fs::read_to_string(path).ok()?;
        if let (Some(client_id), Some(client_secret)) = (
            extract_js_string_assignment(&content, "OAUTH_CLIENT_ID"),
            extract_js_string_assignment(&content, "OAUTH_CLIENT_SECRET"),
        ) {
            return Some(GeminiOAuthClient {
                client_id,
                client_secret,
            });
        }
    }
    None
}

fn extract_js_string_assignment(content: &str, name: &str) -> Option<String> {
    let start = content.find(name)?;
    let assignment = &content[start..];
    let equals = assignment.find('=')?;
    let after_equals = &assignment[equals + 1..];
    let quote_start = after_equals.find('"')?;
    let mut chars = after_equals[quote_start + 1..].chars();
    let mut out = String::new();
    let mut escaped = false;
    for ch in &mut chars {
        if escaped {
            out.push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            return Some(out);
        } else {
            out.push(ch);
        }
    }
    None
}

fn post_bearer_json(client: &Client, url: &str, token: &str, body: &Value) -> Result<Value> {
    let response = client
        .post(url)
        .bearer_auth(token)
        .json(body)
        .send()
        .with_context(|| format!("request failed: {url}"))?;
    let status = response.status();
    let value: Value = response
        .json()
        .with_context(|| format!("failed to parse JSON response from {url}"))?;
    if !status.is_success() {
        bail!(
            "{url} returned HTTP {status}: {}",
            redact_json_for_error(&value)
        );
    }
    Ok(value)
}

pub fn normalize_codex_rate_limit(value: &Value, now_ms: i64) -> Value {
    serde_json::to_value(codex_limit_from_value(value, now_ms)).unwrap_or_else(|_| json!({}))
}

pub fn normalize_gemini_quota_bucket(value: &Value, now_ms: i64) -> Value {
    serde_json::to_value(gemini_limit_from_bucket(value, now_ms)).unwrap_or_else(|_| json!({}))
}

fn codex_limit_from_value(value: &Value, now_ms: i64) -> ModelLimit {
    let id = value
        .get("limitId")
        .and_then(Value::as_str)
        .unwrap_or("codex")
        .to_string();
    let raw_display_name = value
        .get("limitName")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let model = codex_model_name(&id, raw_display_name.as_deref());
    let display_name = raw_display_name.or_else(|| Some(model.clone()));
    let plan_type = value
        .get("planType")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let mut windows = Vec::new();
    if let Some(primary) = value.get("primary") {
        windows.push(codex_window_from_value(
            "primary",
            primary,
            now_ms,
            &model,
            display_name.as_deref(),
        ));
    }
    if let Some(secondary) = value.get("secondary") {
        windows.push(codex_window_from_value(
            "secondary",
            secondary,
            now_ms,
            &model,
            display_name.as_deref(),
        ));
    }
    ModelLimit {
        id,
        model: Some(model),
        display_name,
        plan_type,
        windows,
        credits: value.get("credits").and_then(credits_from_value_opt),
        raw: None,
    }
}

fn codex_model_name(id: &str, display_name: Option<&str>) -> String {
    if display_name
        .map(|name| name.to_ascii_lowercase().contains("spark"))
        .unwrap_or(false)
        || id.to_ascii_lowercase().contains("spark")
        || id.to_ascii_lowercase().contains("bengalfox")
    {
        "Spark".to_string()
    } else if id == "codex" {
        "Total".to_string()
    } else {
        display_name.unwrap_or(id).to_string()
    }
}

fn codex_window_from_value(
    kind: &str,
    value: &Value,
    now_ms: i64,
    model: &str,
    display_name: Option<&str>,
) -> LimitWindowStatus {
    let used_percent = value.get("usedPercent").and_then(number_as_f64);
    let used_percent = used_percent.map(round_percent);
    let remaining_percent = used_percent.map(|used| round_percent((100.0 - used).max(0.0)));
    let resets_at_epoch = value.get("resetsAt").and_then(Value::as_i64);
    let window_minutes = value.get("windowDurationMins").and_then(Value::as_i64);
    let kind = codex_window_label(kind, window_minutes);
    LimitWindowStatus {
        label: Some(codex_usage_limit_label(model, display_name, &kind)),
        kind,
        used_percent,
        remaining_percent,
        remaining_amount: None,
        window_minutes,
        resets_at: resets_at_epoch.map(|seconds| iso_from_seconds(seconds)),
        milliseconds_until_reset: resets_at_epoch.map(|seconds| ((seconds * 1000) - now_ms).max(0)),
    }
}

fn codex_window_label(kind: &str, window_minutes: Option<i64>) -> String {
    match window_minutes {
        Some(300) => "5-hour".to_string(),
        Some(10080) => "weekly".to_string(),
        _ => kind.to_string(),
    }
}

fn codex_usage_limit_label(model: &str, display_name: Option<&str>, kind: &str) -> String {
    let suffix = match kind {
        "5-hour" => "Limite de uso de 5 horas",
        "weekly" => "Limite de uso semanal",
        _ => kind,
    };
    if model == "Total" {
        suffix.to_string()
    } else {
        let prefix = display_name.unwrap_or(model);
        format!("{prefix} {suffix}")
    }
}

fn gemini_limit_from_bucket(value: &Value, now_ms: i64) -> ModelLimit {
    let model_id = value
        .get("modelId")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let remaining_fraction = value.get("remainingFraction").and_then(number_as_f64);
    let remaining_amount = value
        .get("remainingAmount")
        .and_then(Value::as_str)
        .and_then(|v| v.parse::<i64>().ok());
    let reset_time = value
        .get("resetTime")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let reset_ms = reset_time
        .as_deref()
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|date| date.timestamp_millis());

    ModelLimit {
        id: model_id.clone(),
        model: Some(model_id.clone()),
        display_name: None,
        plan_type: None,
        windows: vec![LimitWindowStatus {
            kind: "quota".to_string(),
            label: Some(format!("{model_id} quota")),
            used_percent: remaining_fraction
                .map(|fraction| round_percent((100.0 - fraction * 100.0).max(0.0))),
            remaining_percent: remaining_fraction.map(|fraction| round_percent(fraction * 100.0)),
            remaining_amount,
            window_minutes: None,
            resets_at: reset_time,
            milliseconds_until_reset: reset_ms.map(|reset| (reset - now_ms).max(0)),
        }],
        credits: None,
        raw: None,
    }
}

fn credits_from_value(value: &Value) -> CreditsStatus {
    CreditsStatus {
        has_credits: value.get("hasCredits").and_then(Value::as_bool),
        unlimited: value.get("unlimited").and_then(Value::as_bool),
        balance: value
            .get("balance")
            .and_then(Value::as_str)
            .map(ToString::to_string),
    }
}

fn credits_from_value_opt(value: &Value) -> Option<CreditsStatus> {
    if !value.is_object() {
        return None;
    }
    let credits = credits_from_value(value);
    (credits.has_credits.is_some() || credits.unlimited.is_some() || credits.balance.is_some())
        .then_some(credits)
}

fn gemini_credits_from_available(credits: &[Value]) -> CreditsStatus {
    let mut total = 0_i64;
    let mut has_any = false;
    for credit in credits {
        if let Some(amount) = credit
            .get("creditAmount")
            .and_then(Value::as_str)
            .and_then(|v| v.parse::<i64>().ok())
        {
            total += amount;
            has_any = true;
        }
    }
    CreditsStatus {
        has_credits: Some(has_any),
        unlimited: None,
        balance: has_any.then(|| total.to_string()),
    }
}

fn normalize_codex_usage(value: &Value) -> Value {
    let mut out = serde_json::Map::new();
    if let Some(summary) = value.get("summary") {
        out.insert("summary".to_string(), summary.clone());
    }
    if let Some(buckets) = value.get("dailyUsageBuckets").and_then(Value::as_array) {
        out.insert("daily_bucket_count".to_string(), json!(buckets.len()));
        if let Some(last) = buckets.last() {
            out.insert("latest_daily_bucket".to_string(), last.clone());
        }
    }
    Value::Object(out)
}

pub fn render_human_report(report: &Report) -> String {
    let mut out = String::new();
    out.push_str(&format!("AI limits - {}\n", report.generated_at));
    let mut rendered = false;
    if let Some(codex) = &report.codex {
        out.push_str(&render_provider(codex));
        rendered = true;
    }
    if let Some(gemini) = &report.gemini {
        if rendered {
            out.push('\n');
        }
        out.push_str(&render_provider(gemini));
        rendered = true;
    }
    if !rendered {
        out.push_str("No providers selected\n");
    }
    out
}

fn render_provider(provider: &ProviderStatus) -> String {
    let mut out = String::new();
    out.push_str(&format!("{} [{}]\n", provider.name, provider.status));
    if let Some(version) = &provider.cli_version {
        out.push_str(&format!("  CLI: {version}\n"));
    }
    if let Some(account) = &provider.account {
        out.push_str(&format!("  Account: {account}\n"));
    }
    if let Some(plan) = &provider.plan_type {
        out.push_str(&format!("  Plan: {plan}\n"));
    }
    if let Some(tier) = &provider.tier {
        out.push_str(&format!("  Tier: {tier}\n"));
    }
    if let Some(paid_tier) = &provider.paid_tier {
        out.push_str(&format!("  Paid tier: {paid_tier}\n"));
    }
    if let Some(credits) = &provider.credits {
        out.push_str(&format!("  Credits: {}\n", render_credits(credits)));
    }
    if let Some(error) = &provider.error {
        out.push_str(&format!("  Error: {error}\n"));
    }
    if provider.limits.is_empty() {
        out.push_str("  Limits: none reported\n");
    } else {
        out.push_str("  Limits:\n");
        for limit in &provider.limits {
            let label = limit
                .model
                .as_deref()
                .or(limit.display_name.as_deref())
                .unwrap_or(&limit.id);
            out.push_str(&format!("  - {label} ({})\n", limit.id));
            if let Some(display_name) = &limit.display_name {
                if Some(display_name.as_str()) != limit.model.as_deref() {
                    out.push_str(&format!("    name: {display_name}\n"));
                }
            }
            if let Some(plan) = &limit.plan_type {
                out.push_str(&format!("    plan: {plan}\n"));
            }
            if let Some(credits) = &limit.credits {
                out.push_str(&format!("    credits: {}\n", render_credits(credits)));
            }
            for window in &limit.windows {
                let label = window.label.as_deref().unwrap_or(&window.kind);
                out.push_str(&format!("    {label}: "));
                if let Some(used) = window.used_percent {
                    out.push_str(&format!("{used:.2}% usado, "));
                }
                if let Some(remaining) = window.remaining_percent {
                    out.push_str(&format!("{remaining:.2}% restante, "));
                }
                if let Some(amount) = window.remaining_amount {
                    out.push_str(&format!("{amount} restantes, "));
                }
                if let Some(minutes) = window.window_minutes {
                    out.push_str(&format!("janela de {minutes} min, "));
                }
                if let Some(resets_at) = &window.resets_at {
                    out.push_str(&format!("renova em {resets_at}, "));
                }
                if let Some(ms) = window.milliseconds_until_reset {
                    out.push_str(&format!("{ms} ms para renovar"));
                }
                out.push('\n');
            }
        }
    }
    if let Some(usage) = &provider.usage {
        if !usage.is_null() {
            out.push_str(&format!("  Usage summary: {}\n", compact_json(usage)));
        }
    }
    out
}

fn render_credits(credits: &CreditsStatus) -> String {
    let mut parts = Vec::new();
    if let Some(has_credits) = credits.has_credits {
        parts.push(format!("has_credits={has_credits}"));
    }
    if let Some(unlimited) = credits.unlimited {
        parts.push(format!("unlimited={unlimited}"));
    }
    if let Some(balance) = &credits.balance {
        parts.push(format!("balance={balance}"));
    }
    if parts.is_empty() {
        "not reported".to_string()
    } else {
        parts.join(", ")
    }
}

fn read_cli_version(provider: &str) -> Option<String> {
    let provider = CliProvider::from_name(provider);
    cli_command_candidates(provider)
        .into_iter()
        .filter_map(|command| output_cli_command(&command, &["--version"]).ok())
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .find(|value| !value.is_empty())
}

fn resolve_cli_command(provider: CliProvider) -> PathBuf {
    cli_command_candidates(provider)
        .into_iter()
        .next()
        .unwrap_or_else(|| PathBuf::from(provider.fallback_command()))
}

fn cli_command_candidates(provider: CliProvider) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(value) = env::var_os(provider.env_override()).filter(|value| !value.is_empty()) {
        candidates.push(PathBuf::from(value));
    }
    if let Ok(home) = home_dir() {
        candidates.extend(cli_command_candidates_for_home(provider, &home));
    }
    candidates.push(PathBuf::from(provider.fallback_command()));
    dedupe_paths(candidates)
}

fn cli_command_candidates_for_home(provider: CliProvider, home: &Path) -> Vec<PathBuf> {
    let volta = home.join("AppData").join("Local").join("Volta");
    let mut candidates = Vec::new();
    for node_dir in sorted_volta_node_dirs(&volta) {
        match provider {
            CliProvider::Codex => {
                candidates.push(
                    node_dir
                        .join("node_modules")
                        .join("@openai")
                        .join("codex")
                        .join("node_modules")
                        .join("@openai")
                        .join("codex-win32-x64")
                        .join("vendor")
                        .join("x86_64-pc-windows-msvc")
                        .join("bin")
                        .join("codex.exe"),
                );
                candidates.push(node_dir.join("codex.cmd"));
            }
            CliProvider::Gemini => {
                candidates.push(node_dir.join("gemini.cmd"));
            }
        }
    }
    candidates
        .into_iter()
        .filter(|path| path.is_file())
        .collect()
}

fn sorted_volta_node_dirs(volta: &Path) -> Vec<PathBuf> {
    let node_root = volta.join("tools").join("image").join("node");
    let mut dirs: Vec<_> = fs::read_dir(node_root)
        .ok()
        .into_iter()
        .flat_map(|entries| entries.filter_map(Result::ok))
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort_by(|a, b| {
        semver_path_key(b)
            .cmp(&semver_path_key(a))
            .then_with(|| b.cmp(a))
    });
    dirs
}

fn semver_path_key(path: &Path) -> Vec<u64> {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .split('.')
        .map(|part| part.parse::<u64>().unwrap_or(0))
        .collect()
}

fn dedupe_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for path in paths {
        if !out.iter().any(|existing| existing == &path) {
            out.push(path);
        }
    }
    out
}

fn output_cli_command(command: &Path, args: &[&str]) -> Result<Output> {
    Ok(build_cli_command(command, args).output()?)
}

fn spawn_cli_command(command: &Path, args: &[&str]) -> Result<Child> {
    let mut command = build_cli_command(command, args);
    Ok(command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?)
}

fn build_cli_command(command: &Path, args: &[&str]) -> Command {
    if cfg!(windows) && is_windows_command_script(command) {
        let mut cmd = Command::new("cmd.exe");
        cmd.args(["/d", "/c", "call"]);
        cmd.arg(command);
        cmd.args(args);
        cmd
    } else {
        let mut cmd = Command::new(command);
        cmd.args(args);
        cmd
    }
}

fn is_windows_command_script(command: &Path) -> bool {
    command
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| matches!(value.to_ascii_lowercase().as_str(), "cmd" | "bat"))
        .unwrap_or(false)
}

fn read_codex_account_id() -> Option<String> {
    let path = home_dir().ok()?.join(".codex").join("auth.json");
    let value: Value = read_json_file(&path).ok()?;
    value
        .get("tokens")
        .and_then(|v| v.get("account_id"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

fn read_gemini_account(path: &Path) -> Option<String> {
    let value: Value = read_json_file(path).ok()?;
    value
        .get("active")
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

fn read_json_file<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let content = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&content)?)
}

fn home_dir() -> Result<PathBuf> {
    if let Some(home) = env::var_os("USERPROFILE") {
        return Ok(PathBuf::from(home));
    }
    if let Some(home) = env::var_os("HOME") {
        return Ok(PathBuf::from(home));
    }
    bail!("could not determine home directory")
}

fn number_as_f64(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_i64().map(|v| v as f64))
        .or_else(|| value.as_u64().map(|v| v as f64))
}

fn round_percent(value: f64) -> f64 {
    (value * 100_000.0).round() / 100_000.0
}

fn iso_from_seconds(seconds: i64) -> String {
    DateTime::<Utc>::from_timestamp(seconds, 0)
        .map(|value| value.to_rfc3339_opts(SecondsFormat::Secs, true))
        .unwrap_or_else(|| seconds.to_string())
}

fn iso_from_millis(milliseconds: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(milliseconds)
        .map(|value| value.to_rfc3339_opts(SecondsFormat::Millis, true))
        .unwrap_or_else(|| milliseconds.to_string())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string())
}

fn redact_json_for_error(value: &Value) -> String {
    fn redact(value: &Value) -> Value {
        match value {
            Value::Object(map) => {
                let mut out = serde_json::Map::new();
                for (key, value) in map {
                    if key.to_ascii_lowercase().contains("token")
                        || key.to_ascii_lowercase().contains("secret")
                        || key.to_ascii_lowercase().contains("authorization")
                    {
                        out.insert(key.clone(), Value::String("<redacted>".to_string()));
                    } else {
                        out.insert(key.clone(), redact(value));
                    }
                }
                Value::Object(out)
            }
            Value::Array(values) => Value::Array(values.iter().map(redact).collect()),
            _ => value.clone(),
        }
    }
    compact_json(&redact(value))
}

pub fn report_has_error(report: &Report) -> bool {
    report
        .codex
        .as_ref()
        .is_some_and(|provider| provider.status != "ok")
        || report
            .gemini
            .as_ref()
            .is_some_and(|provider| provider.status != "ok")
}

pub fn validate_report_filters(filters: &ReportFilters) -> Result<()> {
    let invalid: Vec<_> = normalized_filter_terms(&filters.providers)
        .into_iter()
        .filter(|term| term != CliProvider::Codex.key() && term != CliProvider::Gemini.key())
        .collect();
    if invalid.is_empty() {
        Ok(())
    } else {
        bail!(
            "unknown provider filter: {}. Use codex or gemini",
            invalid.join(", ")
        );
    }
}

pub fn apply_report_filters(report: &mut Report, filters: &ReportFilters) {
    if !provider_is_selected(CliProvider::Codex, filters) {
        report.codex = None;
    }
    if !provider_is_selected(CliProvider::Gemini, filters) {
        report.gemini = None;
    }

    let model_terms = normalized_filter_terms(&filters.models);
    if model_terms.is_empty() {
        return;
    }

    if let Some(provider) = &mut report.codex {
        filter_provider_models(provider, &model_terms);
    }
    if let Some(provider) = &mut report.gemini {
        filter_provider_models(provider, &model_terms);
    }
}

fn provider_is_selected(provider: CliProvider, filters: &ReportFilters) -> bool {
    let terms = normalized_filter_terms(&filters.providers);
    terms.is_empty()
        || terms.iter().any(|term| {
            term == provider.key() || term == &provider.display_name().to_ascii_lowercase()
        })
}

fn filter_provider_models(provider: &mut ProviderStatus, terms: &[String]) {
    provider
        .limits
        .retain(|limit| model_matches_terms(limit, terms));
}

fn model_matches_terms(limit: &ModelLimit, terms: &[String]) -> bool {
    terms.iter().any(|term| {
        [
            Some(limit.id.as_str()),
            limit.model.as_deref(),
            limit.display_name.as_deref(),
        ]
        .into_iter()
        .flatten()
        .any(|value| value.to_ascii_lowercase().contains(term))
    })
}

fn normalized_filter_terms(values: &[String]) -> Vec<String> {
    values
        .iter()
        .flat_map(|value| value.split(','))
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalizes_codex_rate_limit_windows() {
        let normalized = normalize_codex_rate_limit(
            &json!({
                "limitId": "codex",
                "limitName": null,
                "planType": "pro",
                "primary": { "usedPercent": 27, "windowDurationMins": 300, "resetsAt": 1781506137 },
                "secondary": { "usedPercent": 41, "windowDurationMins": 10080, "resetsAt": 1781956546 },
                "credits": { "hasCredits": true, "unlimited": false, "balance": "123.4500" }
            }),
            1_781_506_000_000,
        );

        assert_eq!(normalized["id"], "codex");
        assert_eq!(normalized["model"], "Total");
        assert_eq!(normalized["plan_type"], "pro");
        assert_eq!(normalized["windows"][0]["remaining_percent"], 73.0);
        assert_eq!(normalized["windows"][0]["kind"], "5-hour");
        assert_eq!(
            normalized["windows"][0]["label"],
            "Limite de uso de 5 horas"
        );
        assert_eq!(normalized["windows"][1]["kind"], "weekly");
        assert_eq!(normalized["windows"][1]["label"], "Limite de uso semanal");
        assert_eq!(normalized["windows"][1]["window_minutes"], 10080);
        assert_eq!(
            normalized["windows"][0]["resets_at"],
            "2026-06-15T06:48:57Z"
        );
        assert_eq!(
            normalized["windows"][0]["milliseconds_until_reset"],
            137_000
        );
        assert_eq!(normalized["credits"]["balance"], "123.4500");
    }

    #[test]
    fn normalizes_codex_spark_as_separate_model_with_own_windows() {
        let normalized = normalize_codex_rate_limit(
            &json!({
                "limitId": "codex_bengalfox",
                "limitName": "GPT-5.3-Codex-Spark",
                "planType": "pro",
                "primary": { "usedPercent": 0, "windowDurationMins": 300, "resetsAt": 1781512315 },
                "secondary": { "usedPercent": 0, "windowDurationMins": 10080, "resetsAt": 1782099115 }
            }),
            1_781_506_000_000,
        );

        assert_eq!(normalized["id"], "codex_bengalfox");
        assert_eq!(normalized["model"], "Spark");
        assert_eq!(normalized["display_name"], "GPT-5.3-Codex-Spark");
        assert_eq!(normalized["windows"].as_array().unwrap().len(), 2);
        assert_eq!(normalized["windows"][0]["kind"], "5-hour");
        assert_eq!(
            normalized["windows"][0]["label"],
            "GPT-5.3-Codex-Spark Limite de uso de 5 horas"
        );
        assert_eq!(normalized["windows"][1]["kind"], "weekly");
        assert_eq!(
            normalized["windows"][1]["label"],
            "GPT-5.3-Codex-Spark Limite de uso semanal"
        );
    }

    #[test]
    fn normalizes_gemini_quota_fraction_as_percent() {
        let normalized = normalize_gemini_quota_bucket(
            &json!({
                "modelId": "gemini-2.5-pro",
                "remainingFraction": 0.9533333,
                "resetTime": "2026-06-16T02:49:49Z"
            }),
            1_781_506_000_000,
        );

        assert_eq!(normalized["model"], "gemini-2.5-pro");
        assert_eq!(normalized["windows"][0]["remaining_percent"], 95.33333);
        assert_eq!(
            normalized["windows"][0]["resets_at"],
            "2026-06-16T02:49:49Z"
        );
        assert_eq!(
            normalized["windows"][0]["milliseconds_until_reset"],
            72_189_000
        );
    }

    #[test]
    fn human_report_names_providers_without_token_fields() {
        let report = Report {
            generated_at: "2026-06-15T03:30:00Z".to_string(),
            codex: Some(ProviderStatus {
                name: "Codex".to_string(),
                status: "ok".to_string(),
                cli_version: Some("codex-cli 0.139.0".to_string()),
                account: None,
                plan_type: Some("pro".to_string()),
                tier: None,
                paid_tier: None,
                limits: vec![ModelLimit {
                    id: "codex".to_string(),
                    model: None,
                    display_name: None,
                    plan_type: Some("pro".to_string()),
                    windows: vec![LimitWindowStatus {
                        kind: "5-hour".to_string(),
                        label: Some("Limite de uso de 5 horas".to_string()),
                        used_percent: Some(27.0),
                        remaining_percent: Some(73.0),
                        remaining_amount: None,
                        window_minutes: Some(300),
                        resets_at: Some("2026-06-15T06:48:57Z".to_string()),
                        milliseconds_until_reset: Some(137_000),
                    }],
                    credits: None,
                    raw: None,
                }],
                credits: None,
                usage: None,
                source: None,
                error: None,
            }),
            gemini: Some(ProviderStatus {
                name: "Gemini".to_string(),
                status: "ok".to_string(),
                cli_version: Some("0.46.0".to_string()),
                account: None,
                plan_type: None,
                tier: Some("Gemini Code Assist".to_string()),
                paid_tier: None,
                limits: Vec::new(),
                credits: None,
                usage: None,
                source: None,
                error: None,
            }),
        };

        let rendered = render_human_report(&report);

        assert!(rendered.contains("Codex"));
        assert!(rendered.contains("Gemini"));
        assert!(rendered.contains("Limite de uso de 5 horas"));
        assert!(rendered.contains("137000 ms para renovar"));
        assert!(!rendered.to_ascii_lowercase().contains("access_token"));
        assert!(!rendered.to_ascii_lowercase().contains("refresh_token"));
    }

    #[test]
    fn provider_filter_removes_unselected_provider_from_report() {
        let mut report = sample_report();
        apply_report_filters(
            &mut report,
            &ReportFilters {
                providers: vec!["gemini".to_string()],
                models: Vec::new(),
            },
        );

        assert!(report.codex.is_none());
        assert!(report.gemini.is_some());
        assert!(!render_human_report(&report).contains("Codex"));
    }

    #[test]
    fn model_filter_matches_id_model_and_display_name_case_insensitively() {
        let mut report = sample_report();
        apply_report_filters(
            &mut report,
            &ReportFilters {
                providers: Vec::new(),
                models: vec!["spark".to_string()],
            },
        );

        let codex = report.codex.as_ref().unwrap();
        assert_eq!(codex.limits.len(), 1);
        assert_eq!(codex.limits[0].id, "codex_bengalfox");
        let rendered = render_human_report(&report);
        assert!(rendered.contains("Spark"));
        assert!(!rendered.contains("Total (codex)"));
    }

    #[test]
    fn report_error_status_ignores_filtered_out_providers() {
        let report = Report {
            generated_at: "2026-06-15T03:30:00Z".to_string(),
            codex: None,
            gemini: Some(ProviderStatus {
                name: "Gemini".to_string(),
                status: "ok".to_string(),
                cli_version: None,
                account: None,
                plan_type: None,
                tier: None,
                paid_tier: None,
                limits: Vec::new(),
                credits: None,
                usage: None,
                source: None,
                error: None,
            }),
        };

        assert!(!report_has_error(&report));
    }

    #[test]
    fn validates_provider_filter_values() {
        assert!(
            validate_report_filters(&ReportFilters {
                providers: vec!["Codex".to_string(), "gemini".to_string()],
                models: Vec::new(),
            })
            .is_ok()
        );
        assert!(
            validate_report_filters(&ReportFilters {
                providers: vec!["claude".to_string()],
                models: Vec::new(),
            })
            .is_err()
        );
    }

    #[test]
    fn extracts_gemini_oauth_client_from_local_cli_bundle() {
        let home = unique_test_home("gemini-oauth");
        let package_dir = home.join("gemini-cli");
        let bundle_dir = package_dir.join("bundle");
        fs::create_dir_all(&bundle_dir).unwrap();
        fs::write(
            bundle_dir.join("chunk.js"),
            r#"var OAUTH_CLIENT_ID = "test-client-id";
var OAUTH_CLIENT_SECRET = "test-client-secret";"#,
        )
        .unwrap();

        let client = read_gemini_oauth_client_from_package(&package_dir).unwrap();

        assert_eq!(client.client_id, "test-client-id");
        assert_eq!(client.client_secret, "test-client-secret");
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn prefers_current_volta_node_image_codex_over_volta_package_shim() {
        let home = unique_test_home("codex-command");
        let native = home
            .join("AppData")
            .join("Local")
            .join("Volta")
            .join("tools")
            .join("image")
            .join("node")
            .join("24.12.0")
            .join("node_modules")
            .join("@openai")
            .join("codex")
            .join("node_modules")
            .join("@openai")
            .join("codex-win32-x64")
            .join("vendor")
            .join("x86_64-pc-windows-msvc")
            .join("bin")
            .join("codex.exe");
        let shim = home
            .join("AppData")
            .join("Local")
            .join("Volta")
            .join("tools")
            .join("image")
            .join("packages")
            .join("@openai")
            .join("codex")
            .join("codex.cmd");
        fs::create_dir_all(native.parent().unwrap()).unwrap();
        fs::write(&native, "").unwrap();
        fs::create_dir_all(shim.parent().unwrap()).unwrap();
        fs::write(&shim, "").unwrap();

        let candidates = cli_command_candidates_for_home(CliProvider::Codex, &home);

        assert_eq!(candidates[0], native);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn prefers_current_volta_node_image_gemini_over_path_fallback() {
        let home = unique_test_home("gemini-command");
        let current = home
            .join("AppData")
            .join("Local")
            .join("Volta")
            .join("tools")
            .join("image")
            .join("node")
            .join("24.12.0")
            .join("gemini.cmd");
        fs::create_dir_all(current.parent().unwrap()).unwrap();
        fs::write(&current, "").unwrap();

        let candidates = cli_command_candidates_for_home(CliProvider::Gemini, &home);

        assert_eq!(candidates[0], current);
        let _ = fs::remove_dir_all(home);
    }

    #[cfg(windows)]
    #[test]
    fn runs_windows_cmd_scripts_with_arguments() {
        let home = unique_test_home("cmd-script");
        let script = home.join("version.cmd");
        fs::write(&script, "@echo off\r\necho 0.46.0\r\n").unwrap();

        let output = output_cli_command(&script, &["--version"]).unwrap();

        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "0.46.0");
        let _ = fs::remove_dir_all(home);
    }

    fn unique_test_home(name: &str) -> PathBuf {
        let root = env::temp_dir().join(format!("ai-limits-{name}-{}", now_ms()));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn sample_report() -> Report {
        Report {
            generated_at: "2026-06-15T03:30:00Z".to_string(),
            codex: Some(ProviderStatus {
                name: "Codex".to_string(),
                status: "ok".to_string(),
                cli_version: Some("codex-cli 0.139.0".to_string()),
                account: None,
                plan_type: Some("pro".to_string()),
                tier: None,
                paid_tier: None,
                limits: vec![
                    ModelLimit {
                        id: "codex".to_string(),
                        model: Some("Total".to_string()),
                        display_name: Some("Total".to_string()),
                        plan_type: Some("pro".to_string()),
                        windows: Vec::new(),
                        credits: None,
                        raw: None,
                    },
                    ModelLimit {
                        id: "codex_bengalfox".to_string(),
                        model: Some("Spark".to_string()),
                        display_name: Some("GPT-5.3-Codex-Spark".to_string()),
                        plan_type: Some("pro".to_string()),
                        windows: Vec::new(),
                        credits: None,
                        raw: None,
                    },
                ],
                credits: None,
                usage: None,
                source: None,
                error: None,
            }),
            gemini: Some(ProviderStatus {
                name: "Gemini".to_string(),
                status: "ok".to_string(),
                cli_version: Some("0.46.0".to_string()),
                account: None,
                plan_type: None,
                tier: Some("Gemini Code Assist".to_string()),
                paid_tier: None,
                limits: vec![ModelLimit {
                    id: "gemini-2.5-pro".to_string(),
                    model: Some("gemini-2.5-pro".to_string()),
                    display_name: None,
                    plan_type: None,
                    windows: Vec::new(),
                    credits: None,
                    raw: None,
                }],
                credits: None,
                usage: None,
                source: None,
                error: None,
            }),
        }
    }
}
