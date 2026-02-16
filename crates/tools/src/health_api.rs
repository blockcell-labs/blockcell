use async_trait::async_trait;
use blockcell_core::{Error, Result};
use reqwest::Client;
use serde_json::{json, Value};
use tracing::debug;

use crate::{Tool, ToolContext, ToolSchema};

/// Health data tool for Apple Health, Fitbit, and Google Fit.
///
/// Data sources:
/// - **Apple Health**: Parse exported XML files (export.xml from Health app)
/// - **Fitbit**: REST API with OAuth2 token
/// - **Google Fit**: REST API with OAuth2 token
///
/// Supports querying steps, heart rate, sleep, workouts, weight, and more.
pub struct HealthApiTool;

#[async_trait]
impl Tool for HealthApiTool {
    fn schema(&self) -> ToolSchema {
        let str_prop = |desc: &str| -> Value { json!({"type": "string", "description": desc}) };
        let int_prop = |desc: &str| -> Value { json!({"type": "integer", "description": desc}) };

        let mut props = serde_json::Map::new();
        props.insert("source".into(), json!({"type": "string", "enum": ["apple_health", "fitbit", "google_fit"], "description": "Health data source"}));
        props.insert("action".into(), str_prop("Action: summary|steps|heart_rate|sleep|workouts|weight|nutrition|blood_pressure|body_temperature|oxygen_saturation|list_types|export_csv"));
        props.insert("date".into(), str_prop("Date for query (YYYY-MM-DD). Default: today"));
        props.insert("start_date".into(), str_prop("Start date for range queries (YYYY-MM-DD)"));
        props.insert("end_date".into(), str_prop("End date for range queries (YYYY-MM-DD)"));
        props.insert("export_path".into(), str_prop("(apple_health) Path to Apple Health export.xml file"));
        props.insert("data_type".into(), str_prop("Specific health data type identifier (e.g. 'HKQuantityTypeIdentifierStepCount')"));
        props.insert("output_path".into(), str_prop("(export_csv) Output CSV file path"));
        props.insert("limit".into(), int_prop("Maximum number of records to return (default: 100)"));
        props.insert("api_token".into(), str_prop("OAuth2 access token (for Fitbit/Google Fit)"));
        props.insert("user_id".into(), str_prop("(Fitbit) User ID (default: '-' for current user)"));

        ToolSchema {
            name: "health_api",
            description: "Query health and fitness data from Apple Health (XML export parsing), Fitbit API, and Google Fit API. Supports steps, heart rate, sleep, workouts, weight, nutrition, and more. Apple Health requires an exported XML file. Fitbit/Google Fit require OAuth2 tokens via api_token param, config providers section, or environment variables (FITBIT_ACCESS_TOKEN, GOOGLE_FIT_TOKEN).",
            parameters: json!({
                "type": "object",
                "properties": Value::Object(props),
                "required": ["source", "action"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let source = params.get("source").and_then(|v| v.as_str()).unwrap_or("");
        if !["apple_health", "fitbit", "google_fit"].contains(&source) {
            return Err(Error::Tool("source must be 'apple_health', 'fitbit', or 'google_fit'".into()));
        }
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let valid = [
            "summary", "steps", "heart_rate", "sleep", "workouts", "weight",
            "nutrition", "blood_pressure", "body_temperature", "oxygen_saturation",
            "list_types", "export_csv",
        ];
        if !valid.contains(&action) {
            return Err(Error::Tool(format!("Invalid action '{}'. Valid: {}", action, valid.join(", "))));
        }
        if source == "apple_health" && action != "list_types" {
            if params.get("export_path").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                return Err(Error::Tool("'export_path' to Apple Health export.xml is required".into()));
            }
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let source = params["source"].as_str().unwrap_or("");
        let action = params["action"].as_str().unwrap_or("");

        match source {
            "apple_health" => self.execute_apple_health(&ctx, &params, action).await,
            "fitbit" => self.execute_fitbit(&ctx, &params, action).await,
            "google_fit" => self.execute_google_fit(&ctx, &params, action).await,
            _ => Err(Error::Tool(format!("Unknown source: {}", source))),
        }
    }
}

impl HealthApiTool {
    // ─── Apple Health (XML parsing) ───

    async fn execute_apple_health(&self, ctx: &ToolContext, params: &Value, action: &str) -> Result<Value> {
        if action == "list_types" {
            return Ok(json!({
                "common_types": [
                    "HKQuantityTypeIdentifierStepCount",
                    "HKQuantityTypeIdentifierHeartRate",
                    "HKQuantityTypeIdentifierBodyMass",
                    "HKQuantityTypeIdentifierHeight",
                    "HKQuantityTypeIdentifierActiveEnergyBurned",
                    "HKQuantityTypeIdentifierBasalEnergyBurned",
                    "HKQuantityTypeIdentifierDistanceWalkingRunning",
                    "HKQuantityTypeIdentifierFlightsClimbed",
                    "HKQuantityTypeIdentifierBloodPressureSystolic",
                    "HKQuantityTypeIdentifierBloodPressureDiastolic",
                    "HKQuantityTypeIdentifierBodyTemperature",
                    "HKQuantityTypeIdentifierOxygenSaturation",
                    "HKQuantityTypeIdentifierDietaryEnergyConsumed",
                    "HKQuantityTypeIdentifierDietaryProtein",
                    "HKQuantityTypeIdentifierDietaryCarbohydrates",
                    "HKQuantityTypeIdentifierDietaryFatTotal",
                    "HKCategoryTypeIdentifierSleepAnalysis",
                    "HKWorkoutTypeIdentifier"
                ],
                "note": "Use these type identifiers with the 'data_type' parameter for specific queries."
            }));
        }

        let export_path = params.get("export_path").and_then(|v| v.as_str()).unwrap_or("");
        let resolved_path = resolve_path(ctx, export_path);

        // Parse dates
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let date = params.get("date").and_then(|v| v.as_str()).unwrap_or(&today);
        let start_date = params.get("start_date").and_then(|v| v.as_str()).unwrap_or(date);
        let end_date = params.get("end_date").and_then(|v| v.as_str()).unwrap_or(date);
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(100);

        // Map action to HK type
        let hk_type = match action {
            "steps" => "HKQuantityTypeIdentifierStepCount",
            "heart_rate" => "HKQuantityTypeIdentifierHeartRate",
            "weight" => "HKQuantityTypeIdentifierBodyMass",
            "blood_pressure" => "HKQuantityTypeIdentifierBloodPressureSystolic",
            "body_temperature" => "HKQuantityTypeIdentifierBodyTemperature",
            "oxygen_saturation" => "HKQuantityTypeIdentifierOxygenSaturation",
            "sleep" => "HKCategoryTypeIdentifierSleepAnalysis",
            "workouts" => "HKWorkoutTypeIdentifier",
            "nutrition" => "HKQuantityTypeIdentifierDietaryEnergyConsumed",
            "summary" | "export_csv" => "", // handled separately
            _ => params.get("data_type").and_then(|v| v.as_str()).unwrap_or(""),
        };

        if action == "export_csv" {
            return self.apple_health_export_csv(ctx, &resolved_path, hk_type, start_date, end_date, params).await;
        }

        if action == "summary" {
            return self.apple_health_summary(&resolved_path, date).await;
        }

        // Use Python + xml.etree for efficient XML parsing (export.xml can be huge)
        let script = format!(
            r#"
import xml.etree.ElementTree as ET
import json, sys

tree = ET.iterparse("{path}", events=('end',))
records = []
count = 0
target_type = "{hk_type}"
start = "{start}"
end = "{end}"
limit = {limit}

for event, elem in tree:
    if elem.tag == 'Record' and (target_type == "" or elem.get('type') == target_type):
        date = elem.get('startDate', '')[:10]
        if date >= start and date <= end:
            records.append({{
                'type': elem.get('type'),
                'value': elem.get('value'),
                'unit': elem.get('unit'),
                'startDate': elem.get('startDate'),
                'endDate': elem.get('endDate'),
                'sourceName': elem.get('sourceName'),
            }})
            count += 1
            if count >= limit:
                break
    elif elem.tag == 'Workout' and target_type == 'HKWorkoutTypeIdentifier':
        date = elem.get('startDate', '')[:10]
        if date >= start and date <= end:
            records.append({{
                'type': 'Workout',
                'activityType': elem.get('workoutActivityType'),
                'duration': elem.get('duration'),
                'durationUnit': elem.get('durationUnit'),
                'totalDistance': elem.get('totalDistance'),
                'totalEnergyBurned': elem.get('totalEnergyBurned'),
                'startDate': elem.get('startDate'),
                'endDate': elem.get('endDate'),
                'sourceName': elem.get('sourceName'),
            }})
            count += 1
            if count >= limit:
                break
    elem.clear()

print(json.dumps({{"records": records, "count": len(records), "type": target_type, "date_range": f"{{start}} to {{end}}"}}))
"#,
            path = resolved_path.replace('"', r#"\""#),
            hk_type = hk_type,
            start = start_date,
            end = end_date,
            limit = limit,
        );

        self.run_python_script(&script).await
    }

    async fn apple_health_summary(&self, path: &str, date: &str) -> Result<Value> {
        let script = format!(
            r#"
import xml.etree.ElementTree as ET
import json

path = "{path}"
date = "{date}"
summary = {{"steps": 0, "heart_rate_readings": [], "active_calories": 0, "distance_km": 0, "flights_climbed": 0, "workouts": []}}

for event, elem in ET.iterparse(path, events=('end',)):
    if elem.tag == 'Record':
        d = elem.get('startDate', '')[:10]
        if d == date:
            t = elem.get('type', '')
            v = elem.get('value', '0')
            try:
                val = float(v)
            except:
                val = 0
            if t == 'HKQuantityTypeIdentifierStepCount':
                summary['steps'] += int(val)
            elif t == 'HKQuantityTypeIdentifierHeartRate':
                summary['heart_rate_readings'].append(val)
            elif t == 'HKQuantityTypeIdentifierActiveEnergyBurned':
                summary['active_calories'] += val
            elif t == 'HKQuantityTypeIdentifierDistanceWalkingRunning':
                summary['distance_km'] += val
            elif t == 'HKQuantityTypeIdentifierFlightsClimbed':
                summary['flights_climbed'] += int(val)
    elif elem.tag == 'Workout':
        d = elem.get('startDate', '')[:10]
        if d == date:
            summary['workouts'].append({{
                'activity': elem.get('workoutActivityType', '').replace('HKWorkoutActivityType', ''),
                'duration_min': round(float(elem.get('duration', '0')), 1),
                'calories': round(float(elem.get('totalEnergyBurned', '0')), 1),
            }})
    elem.clear()

hr = summary['heart_rate_readings']
if hr:
    summary['heart_rate_avg'] = round(sum(hr) / len(hr), 1)
    summary['heart_rate_min'] = round(min(hr), 1)
    summary['heart_rate_max'] = round(max(hr), 1)
    summary['heart_rate_count'] = len(hr)
del summary['heart_rate_readings']
summary['active_calories'] = round(summary['active_calories'], 1)
summary['distance_km'] = round(summary['distance_km'], 2)
summary['date'] = date

print(json.dumps(summary))
"#,
            path = path.replace('"', r#"\""#),
            date = date,
        );

        self.run_python_script(&script).await
    }

    async fn apple_health_export_csv(&self, ctx: &ToolContext, path: &str, hk_type: &str, start: &str, end: &str, params: &Value) -> Result<Value> {
        let output = params.get("output_path").and_then(|v| v.as_str())
            .map(|p| resolve_path(ctx, p))
            .unwrap_or_else(|| {
                let media_dir = ctx.workspace.join("media");
                let _ = std::fs::create_dir_all(&media_dir);
                media_dir.join(format!("health_export_{}.csv", chrono::Utc::now().format("%Y%m%d_%H%M%S")))
                    .to_string_lossy().to_string()
            });

        let script = format!(
            r#"
import xml.etree.ElementTree as ET
import csv, json

path = "{path}"
output = "{output}"
target = "{hk_type}"
start = "{start}"
end = "{end}"

with open(output, 'w', newline='') as f:
    writer = csv.writer(f)
    writer.writerow(['type', 'value', 'unit', 'startDate', 'endDate', 'sourceName'])
    count = 0
    for event, elem in ET.iterparse(path, events=('end',)):
        if elem.tag == 'Record':
            t = elem.get('type', '')
            if target and t != target:
                elem.clear()
                continue
            d = elem.get('startDate', '')[:10]
            if d >= start and d <= end:
                writer.writerow([t, elem.get('value',''), elem.get('unit',''), elem.get('startDate',''), elem.get('endDate',''), elem.get('sourceName','')])
                count += 1
        elem.clear()

print(json.dumps({{"output": output, "records_exported": count}}))
"#,
            path = path.replace('"', r#"\""#),
            output = output.replace('"', r#"\""#),
            hk_type = hk_type,
            start = start,
            end = end,
        );

        self.run_python_script(&script).await
    }

    // ─── Fitbit ───

    async fn execute_fitbit(&self, ctx: &ToolContext, params: &Value, action: &str) -> Result<Value> {
        let token = params.get("api_token").and_then(|v| v.as_str()).map(String::from)
            .or_else(|| ctx.config.providers.get("fitbit").map(|p| p.api_key.clone()))
            .or_else(|| std::env::var("FITBIT_ACCESS_TOKEN").ok())
            .unwrap_or_default();

        if token.is_empty() {
            return Err(Error::Tool("Fitbit OAuth2 access token is required. Set via api_token, config providers.fitbit.api_key, or FITBIT_ACCESS_TOKEN env var.".into()));
        }

        let user_id = params.get("user_id").and_then(|v| v.as_str()).unwrap_or("-");
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let date = params.get("date").and_then(|v| v.as_str()).unwrap_or(&today);
        let client = Client::new();
        let base = format!("https://api.fitbit.com/1/user/{}", user_id);

        let url = match action {
            "summary" => format!("{}/activities/date/{}.json", base, date),
            "steps" => format!("{}/activities/steps/date/{}/1d.json", base, date),
            "heart_rate" => format!("{}/activities/heart/date/{}/1d.json", base, date),
            "sleep" => format!("{}/sleep/date/{}.json", base, date),
            "workouts" => format!("{}/activities/date/{}.json", base, date),
            "weight" => {
                let start = params.get("start_date").and_then(|v| v.as_str()).unwrap_or(date);
                let end = params.get("end_date").and_then(|v| v.as_str()).unwrap_or(date);
                format!("{}/body/log/weight/date/{}/{}.json", base, start, end)
            }
            "nutrition" => format!("{}/foods/log/date/{}.json", base, date),
            "blood_pressure" | "body_temperature" | "oxygen_saturation" => {
                return Err(Error::Tool(format!("'{}' is not directly available via Fitbit API. Use the Fitbit app or web dashboard.", action)));
            }
            "list_types" => {
                return Ok(json!({
                    "available_actions": ["summary", "steps", "heart_rate", "sleep", "workouts", "weight", "nutrition"],
                    "note": "Fitbit API provides these data types. Ensure your OAuth2 token has the required scopes."
                }));
            }
            "export_csv" => {
                return Err(Error::Tool("export_csv is only supported for apple_health source".into()));
            }
            _ => return Err(Error::Tool(format!("Unknown Fitbit action: {}", action))),
        };

        debug!(url = %url, "Fitbit API");
        let resp = client.get(&url)
            .header("Authorization", format!("Bearer {}", token))
            .send()
            .await
            .map_err(|e| Error::Tool(format!("Fitbit API request failed: {}", e)))?;

        let status = resp.status();
        let body: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse Fitbit response: {}", e)))?;

        if !status.is_success() {
            return Err(Error::Tool(format!("Fitbit API error ({}): {:?}", status, body)));
        }
        Ok(body)
    }

    // ─── Google Fit ───

    async fn execute_google_fit(&self, ctx: &ToolContext, params: &Value, action: &str) -> Result<Value> {
        let token = params.get("api_token").and_then(|v| v.as_str()).map(String::from)
            .or_else(|| ctx.config.providers.get("google_fit").map(|p| p.api_key.clone()))
            .or_else(|| std::env::var("GOOGLE_FIT_TOKEN").ok())
            .unwrap_or_default();

        if token.is_empty() {
            return Err(Error::Tool("Google Fit OAuth2 access token is required. Set via api_token, config providers.google_fit.api_key, or GOOGLE_FIT_TOKEN env var.".into()));
        }

        let client = Client::new();
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let date = params.get("date").and_then(|v| v.as_str()).unwrap_or(&today);
        let start_date = params.get("start_date").and_then(|v| v.as_str()).unwrap_or(date);
        let end_date = params.get("end_date").and_then(|v| v.as_str()).unwrap_or(date);

        // Convert dates to nanosecond timestamps
        let start_ns = date_to_nanos(start_date)?;
        let end_ns = date_to_nanos_end(end_date)?;

        let data_type = match action {
            "steps" => "com.google.step_count.delta",
            "heart_rate" => "com.google.heart_rate.bpm",
            "sleep" => "com.google.sleep.segment",
            "weight" => "com.google.weight",
            "nutrition" => "com.google.nutrition",
            "workouts" => "com.google.activity.segment",
            "blood_pressure" => "com.google.blood_pressure",
            "body_temperature" => "com.google.body.temperature",
            "oxygen_saturation" => "com.google.oxygen_saturation",
            "summary" => {
                // Aggregate multiple data types
                let types = ["com.google.step_count.delta", "com.google.calories.expended", "com.google.distance.delta", "com.google.heart_rate.bpm"];
                let mut summary = json!({"date": date});
                for dt in &types {
                    let url = format!(
                        "https://www.googleapis.com/fitness/v1/users/me/dataset:aggregate"
                    );
                    let body = json!({
                        "aggregateBy": [{"dataTypeName": dt}],
                        "startTimeMillis": start_ns / 1_000_000,
                        "endTimeMillis": end_ns / 1_000_000,
                    });
                    if let Ok(resp) = Self::gfit_post(&client, &url, &token, &body).await {
                        let key = dt.split('.').nth(2).unwrap_or("unknown");
                        summary[key] = resp;
                    }
                }
                return Ok(summary);
            }
            "list_types" => {
                let url = "https://www.googleapis.com/fitness/v1/users/me/dataSources";
                return Self::gfit_get(&client, url, &token).await;
            }
            "export_csv" => {
                return Err(Error::Tool("export_csv is only supported for apple_health source".into()));
            }
            _ => return Err(Error::Tool(format!("Unknown Google Fit action: {}", action))),
        };

        // Use aggregate endpoint for most queries
        let url = "https://www.googleapis.com/fitness/v1/users/me/dataset:aggregate";
        let body = json!({
            "aggregateBy": [{"dataTypeName": data_type}],
            "startTimeMillis": start_ns / 1_000_000,
            "endTimeMillis": end_ns / 1_000_000,
        });

        Self::gfit_post(&client, url, &token, &body).await
    }

    async fn gfit_get(client: &Client, url: &str, token: &str) -> Result<Value> {
        let resp = client.get(url)
            .header("Authorization", format!("Bearer {}", token))
            .send()
            .await
            .map_err(|e| Error::Tool(format!("Google Fit request failed: {}", e)))?;
        let status = resp.status();
        let body: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;
        if !status.is_success() {
            return Err(Error::Tool(format!("Google Fit error ({}): {:?}", status, body)));
        }
        Ok(body)
    }

    async fn gfit_post(client: &Client, url: &str, token: &str, payload: &Value) -> Result<Value> {
        let resp = client.post(url)
            .header("Authorization", format!("Bearer {}", token))
            .json(payload)
            .send()
            .await
            .map_err(|e| Error::Tool(format!("Google Fit request failed: {}", e)))?;
        let status = resp.status();
        let body: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;
        if !status.is_success() {
            return Err(Error::Tool(format!("Google Fit error ({}): {:?}", status, body)));
        }
        Ok(body)
    }

    // ─── Helpers ───

    async fn run_python_script(&self, script: &str) -> Result<Value> {
        let output = tokio::process::Command::new("python3")
            .arg("-c")
            .arg(script)
            .output()
            .await
            .map_err(|e| Error::Tool(format!("Failed to run Python: {}", e)))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if !output.status.success() {
            return Err(Error::Tool(format!("Python script failed: {}", stderr)));
        }

        serde_json::from_str(&stdout)
            .map_err(|e| Error::Tool(format!("Failed to parse Python output: {}. Output: {}", e, stdout)))
    }
}

fn resolve_path(ctx: &ToolContext, path: &str) -> String {
    if path.starts_with('/') {
        path.to_string()
    } else if path.starts_with("~/") {
        dirs::home_dir()
            .map(|h| h.join(&path[2..]).to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string())
    } else {
        ctx.workspace.join(path).to_string_lossy().to_string()
    }
}

fn date_to_nanos(date_str: &str) -> Result<i64> {
    let dt = chrono::NaiveDate::parse_from_str(date_str, "%Y-%m-%d")
        .map_err(|e| Error::Tool(format!("Invalid date '{}': {}", date_str, e)))?;
    let datetime = dt.and_hms_opt(0, 0, 0)
        .ok_or_else(|| Error::Tool("Failed to create datetime".into()))?;
    Ok(datetime.and_utc().timestamp_nanos_opt().unwrap_or(0))
}

fn date_to_nanos_end(date_str: &str) -> Result<i64> {
    let dt = chrono::NaiveDate::parse_from_str(date_str, "%Y-%m-%d")
        .map_err(|e| Error::Tool(format!("Invalid date '{}': {}", date_str, e)))?;
    let datetime = dt.and_hms_opt(23, 59, 59)
        .ok_or_else(|| Error::Tool("Failed to create datetime".into()))?;
    Ok(datetime.and_utc().timestamp_nanos_opt().unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema() {
        let tool = HealthApiTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "health_api");
        assert!(schema.description.contains("Apple Health"));
        assert!(schema.description.contains("Fitbit"));
        assert!(schema.description.contains("Google Fit"));
    }

    #[test]
    fn test_validate_valid() {
        let tool = HealthApiTool;
        assert!(tool.validate(&json!({"source": "apple_health", "action": "list_types"})).is_ok());
        assert!(tool.validate(&json!({"source": "apple_health", "action": "steps", "export_path": "/path/to/export.xml"})).is_ok());
        assert!(tool.validate(&json!({"source": "fitbit", "action": "summary"})).is_ok());
        assert!(tool.validate(&json!({"source": "google_fit", "action": "steps"})).is_ok());
    }

    #[test]
    fn test_validate_apple_needs_path() {
        let tool = HealthApiTool;
        assert!(tool.validate(&json!({"source": "apple_health", "action": "steps"})).is_err());
    }

    #[test]
    fn test_validate_invalid_source() {
        let tool = HealthApiTool;
        assert!(tool.validate(&json!({"source": "garmin", "action": "steps"})).is_err());
    }

    #[test]
    fn test_validate_invalid_action() {
        let tool = HealthApiTool;
        assert!(tool.validate(&json!({"source": "fitbit", "action": "invalid"})).is_err());
    }

    #[test]
    fn test_date_to_nanos() {
        assert!(date_to_nanos("2025-01-15").is_ok());
        assert!(date_to_nanos("invalid").is_err());
    }

    #[test]
    fn test_date_to_nanos_end() {
        let start = date_to_nanos("2025-01-15").unwrap();
        let end = date_to_nanos_end("2025-01-15").unwrap();
        assert!(end > start);
    }
}
