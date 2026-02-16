use async_trait::async_trait;
use blockcell_core::{Error, Result};
use serde_json::{json, Value};
use std::path::PathBuf;

use crate::{Tool, ToolContext, ToolSchema};

fn expand_path(path: &str, workspace: &PathBuf) -> PathBuf {
    if path.starts_with("~/") {
        dirs::home_dir()
            .map(|h| h.join(&path[2..]))
            .unwrap_or_else(|| PathBuf::from(path))
    } else if path.starts_with('/') {
        PathBuf::from(path)
    } else {
        workspace.join(path)
    }
}

pub struct DataProcessTool;

#[async_trait]
impl Tool for DataProcessTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "data_process",
            description: "Structured data processing: read/write CSV, data cleaning, aggregation, statistics, filtering, sorting, and export. Works with CSV files and in-memory JSON data.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["read_csv", "write_csv", "query", "stats", "transform"],
                        "description": "Action to perform"
                    },
                    "path": {
                        "type": "string",
                        "description": "(read_csv/write_csv) Path to CSV file"
                    },
                    "delimiter": {
                        "type": "string",
                        "description": "(read_csv/write_csv) Delimiter character, default ','"
                    },
                    "has_header": {
                        "type": "boolean",
                        "description": "(read_csv) Whether the CSV has a header row, default true"
                    },
                    "data": {
                        "type": "array",
                        "description": "(write_csv/query/stats/transform) Array of objects (rows)"
                    },
                    "columns": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "(query) Columns to select. Empty = all columns."
                    },
                    "filter": {
                        "type": "object",
                        "description": "(query) Filter conditions. Keys are column names, values are match values. Supports operators: {\"age\": {\"gt\": 18}}, {\"name\": {\"contains\": \"John\"}}, {\"status\": \"active\"}"
                    },
                    "sort_by": {
                        "type": "string",
                        "description": "(query) Column to sort by"
                    },
                    "sort_order": {
                        "type": "string",
                        "enum": ["asc", "desc"],
                        "description": "(query) Sort order, default 'asc'"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "(query/read_csv) Max rows to return"
                    },
                    "group_by": {
                        "type": "string",
                        "description": "(stats) Column to group by"
                    },
                    "agg_column": {
                        "type": "string",
                        "description": "(stats) Column to aggregate"
                    },
                    "agg_func": {
                        "type": "string",
                        "enum": ["count", "sum", "avg", "min", "max", "distinct", "median", "percentile", "correlation"],
                        "description": "(stats) Aggregation function. 'percentile' requires 'percentile_value'. 'correlation' requires 'correlation_column'."
                    },
                    "percentile_value": {
                        "type": "number",
                        "description": "(stats, agg_func=percentile) Percentile to compute, 0-100. E.g. 25 for Q1, 50 for median, 75 for Q3, 90 for P90."
                    },
                    "correlation_column": {
                        "type": "string",
                        "description": "(stats, agg_func=correlation) Second column to compute Pearson correlation with agg_column."
                    },
                    "transform_ops": {
                        "type": "array",
                        "description": "(transform) Array of transform operations: [{\"op\": \"rename\", \"from\": \"old\", \"to\": \"new\"}, {\"op\": \"drop\", \"columns\": [\"col1\"]}, {\"op\": \"fill_null\", \"column\": \"col\", \"value\": \"default\"}, {\"op\": \"dedup\", \"columns\": [\"col1\"]}, {\"op\": \"add_column\", \"name\": \"new_col\", \"value\": \"constant\"}, {\"op\": \"to_number\", \"column\": \"col\"}]"
                    },
                    "output_path": {
                        "type": "string",
                        "description": "(query/stats/transform) Optional: write result to this CSV path"
                    }
                },
                "required": ["action"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Validation("Missing required parameter: action".to_string()))?;

        match action {
            "read_csv" => {
                if params.get("path").and_then(|v| v.as_str()).is_none() {
                    return Err(Error::Validation("read_csv requires 'path'".to_string()));
                }
            }
            "write_csv" => {
                if params.get("path").and_then(|v| v.as_str()).is_none() {
                    return Err(Error::Validation("write_csv requires 'path'".to_string()));
                }
                if params.get("data").and_then(|v| v.as_array()).is_none() {
                    return Err(Error::Validation("write_csv requires 'data' array".to_string()));
                }
            }
            "query" | "transform" => {
                let has_data = params.get("data").and_then(|v| v.as_array()).is_some();
                let has_path = params.get("path").and_then(|v| v.as_str()).is_some();
                if !has_data && !has_path {
                    return Err(Error::Validation(format!("{} requires 'data' array or 'path' to a CSV", action)));
                }
            }
            "stats" => {
                let has_data = params.get("data").and_then(|v| v.as_array()).is_some();
                let has_path = params.get("path").and_then(|v| v.as_str()).is_some();
                if !has_data && !has_path {
                    return Err(Error::Validation("stats requires 'data' array or 'path' to a CSV".to_string()));
                }
            }
            _ => return Err(Error::Validation(format!("Unknown action: {}", action))),
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let action = params["action"].as_str().unwrap();
        let workspace = ctx.workspace.clone();

        match action {
            "read_csv" => {
                let ws = workspace.clone();
                let p = params.clone();
                tokio::task::spawn_blocking(move || action_read_csv(&ws, &p))
                    .await
                    .map_err(|e| Error::Tool(format!("CSV read failed: {}", e)))?
            }
            "write_csv" => {
                let ws = workspace.clone();
                let p = params.clone();
                tokio::task::spawn_blocking(move || action_write_csv(&ws, &p))
                    .await
                    .map_err(|e| Error::Tool(format!("CSV write failed: {}", e)))?
            }
            "query" => {
                let ws = workspace.clone();
                let p = params.clone();
                tokio::task::spawn_blocking(move || action_query(&ws, &p))
                    .await
                    .map_err(|e| Error::Tool(format!("Query failed: {}", e)))?
            }
            "stats" => {
                let ws = workspace.clone();
                let p = params.clone();
                tokio::task::spawn_blocking(move || action_stats(&ws, &p))
                    .await
                    .map_err(|e| Error::Tool(format!("Stats failed: {}", e)))?
            }
            "transform" => {
                let ws = workspace.clone();
                let p = params.clone();
                tokio::task::spawn_blocking(move || action_transform(&ws, &p))
                    .await
                    .map_err(|e| Error::Tool(format!("Transform failed: {}", e)))?
            }
            _ => Err(Error::Tool(format!("Unknown action: {}", action))),
        }
    }
}

/// Load data from either inline 'data' param or from a CSV file at 'path'.
fn load_data(workspace: &PathBuf, params: &Value) -> Result<Vec<Value>> {
    if let Some(data) = params.get("data").and_then(|v| v.as_array()) {
        return Ok(data.clone());
    }
    if let Some(path_str) = params.get("path").and_then(|v| v.as_str()) {
        let path = expand_path(path_str, workspace);
        let delimiter = params.get("delimiter").and_then(|v| v.as_str()).unwrap_or(",");
        let has_header = params.get("has_header").and_then(|v| v.as_bool()).unwrap_or(true);
        return read_csv_to_json(&path, delimiter, has_header);
    }
    Err(Error::Tool("No data source provided".to_string()))
}

fn read_csv_to_json(path: &PathBuf, delimiter: &str, has_header: bool) -> Result<Vec<Value>> {
    if !path.exists() {
        return Err(Error::NotFound(format!("CSV file not found: {}", path.display())));
    }

    let content = std::fs::read_to_string(path)?;
    let delim = delimiter.as_bytes().first().copied().unwrap_or(b',');

    let mut rdr = csv::ReaderBuilder::new()
        .delimiter(delim)
        .has_headers(has_header)
        .flexible(true)
        .from_reader(content.as_bytes());

    let headers: Vec<String> = if has_header {
        rdr.headers()
            .map_err(|e| Error::Tool(format!("CSV header error: {}", e)))?
            .iter()
            .map(|h| h.to_string())
            .collect()
    } else {
        // Generate column names: col0, col1, ...
        let first_record = rdr.records().next();
        if let Some(Ok(record)) = first_record {
            (0..record.len()).map(|i| format!("col{}", i)).collect()
        } else {
            return Ok(vec![]);
        }
    };

    let mut rows = Vec::new();

    // If no header, we need to re-read from scratch
    if !has_header {
        let mut rdr2 = csv::ReaderBuilder::new()
            .delimiter(delim)
            .has_headers(false)
            .flexible(true)
            .from_reader(content.as_bytes());

        for result in rdr2.records() {
            let record = result.map_err(|e| Error::Tool(format!("CSV parse error: {}", e)))?;
            let mut row = serde_json::Map::new();
            for (i, field) in record.iter().enumerate() {
                let key = headers.get(i).cloned().unwrap_or_else(|| format!("col{}", i));
                row.insert(key, try_parse_value(field));
            }
            rows.push(Value::Object(row));
        }
    } else {
        for result in rdr.records() {
            let record = result.map_err(|e| Error::Tool(format!("CSV parse error: {}", e)))?;
            let mut row = serde_json::Map::new();
            for (i, field) in record.iter().enumerate() {
                let key = headers.get(i).cloned().unwrap_or_else(|| format!("col{}", i));
                row.insert(key, try_parse_value(field));
            }
            rows.push(Value::Object(row));
        }
    }

    Ok(rows)
}

/// Try to parse a string value as number or boolean, fall back to string.
fn try_parse_value(s: &str) -> Value {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Value::Null;
    }
    if let Ok(i) = trimmed.parse::<i64>() {
        return json!(i);
    }
    if let Ok(f) = trimmed.parse::<f64>() {
        return json!(f);
    }
    if trimmed.eq_ignore_ascii_case("true") {
        return json!(true);
    }
    if trimmed.eq_ignore_ascii_case("false") {
        return json!(false);
    }
    json!(s)
}

fn action_read_csv(workspace: &PathBuf, params: &Value) -> Result<Value> {
    let path_str = params["path"].as_str().unwrap();
    let path = expand_path(path_str, workspace);
    let delimiter = params.get("delimiter").and_then(|v| v.as_str()).unwrap_or(",");
    let has_header = params.get("has_header").and_then(|v| v.as_bool()).unwrap_or(true);
    let limit = params.get("limit").and_then(|v| v.as_u64());

    let mut rows = read_csv_to_json(&path, delimiter, has_header)?;
    let total = rows.len();

    if let Some(lim) = limit {
        rows.truncate(lim as usize);
    }

    // Extract column names from first row
    let columns: Vec<String> = rows.first()
        .and_then(|r| r.as_object())
        .map(|obj| obj.keys().cloned().collect())
        .unwrap_or_default();

    Ok(json!({
        "path": path.display().to_string(),
        "total_rows": total,
        "returned_rows": rows.len(),
        "columns": columns,
        "data": rows
    }))
}

fn action_write_csv(workspace: &PathBuf, params: &Value) -> Result<Value> {
    let path_str = params["path"].as_str().unwrap();
    let path = expand_path(path_str, workspace);
    let delimiter = params.get("delimiter").and_then(|v| v.as_str()).unwrap_or(",");
    let data = params["data"].as_array().unwrap();

    if data.is_empty() {
        return Err(Error::Tool("Cannot write empty data".to_string()));
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    write_json_to_csv(&path, data, delimiter)?;

    Ok(json!({
        "status": "written",
        "path": path.display().to_string(),
        "rows": data.len()
    }))
}

fn write_json_to_csv(path: &PathBuf, data: &[Value], delimiter: &str) -> Result<()> {
    let delim = delimiter.as_bytes().first().copied().unwrap_or(b',');

    // Collect all column names from all rows (preserving order from first row)
    let mut columns: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for row in data {
        if let Some(obj) = row.as_object() {
            for key in obj.keys() {
                if seen.insert(key.clone()) {
                    columns.push(key.clone());
                }
            }
        }
    }

    let mut wtr = csv::WriterBuilder::new()
        .delimiter(delim)
        .from_path(path)
        .map_err(|e| Error::Tool(format!("CSV write error: {}", e)))?;

    // Write header
    wtr.write_record(&columns)
        .map_err(|e| Error::Tool(format!("CSV header write error: {}", e)))?;

    // Write rows
    for row in data {
        let record: Vec<String> = columns.iter().map(|col| {
            match row.get(col) {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Number(n)) => n.to_string(),
                Some(Value::Bool(b)) => b.to_string(),
                Some(Value::Null) | None => String::new(),
                Some(v) => v.to_string(),
            }
        }).collect();
        wtr.write_record(&record)
            .map_err(|e| Error::Tool(format!("CSV row write error: {}", e)))?;
    }

    wtr.flush().map_err(|e| Error::Tool(format!("CSV flush error: {}", e)))?;
    Ok(())
}

fn action_query(workspace: &PathBuf, params: &Value) -> Result<Value> {
    let mut data = load_data(workspace, params)?;
    let total = data.len();

    // Filter
    if let Some(filter) = params.get("filter").and_then(|v| v.as_object()) {
        data = data.into_iter().filter(|row| {
            filter.iter().all(|(col, condition)| {
                let val = row.get(col);
                match_filter(val, condition)
            })
        }).collect();
    }

    // Sort
    if let Some(sort_col) = params.get("sort_by").and_then(|v| v.as_str()) {
        let desc = params.get("sort_order").and_then(|v| v.as_str()) == Some("desc");
        data.sort_by(|a, b| {
            let va = a.get(sort_col);
            let vb = b.get(sort_col);
            let cmp = compare_values(va, vb);
            if desc { cmp.reverse() } else { cmp }
        });
    }

    // Select columns
    if let Some(cols) = params.get("columns").and_then(|v| v.as_array()) {
        let col_names: Vec<&str> = cols.iter().filter_map(|c| c.as_str()).collect();
        if !col_names.is_empty() {
            data = data.into_iter().map(|row| {
                let mut new_row = serde_json::Map::new();
                if let Some(obj) = row.as_object() {
                    for col in &col_names {
                        if let Some(v) = obj.get(*col) {
                            new_row.insert(col.to_string(), v.clone());
                        }
                    }
                }
                Value::Object(new_row)
            }).collect();
        }
    }

    // Limit
    if let Some(lim) = params.get("limit").and_then(|v| v.as_u64()) {
        data.truncate(lim as usize);
    }

    let result = json!({
        "total_rows": total,
        "filtered_rows": data.len(),
        "data": data
    });

    // Optional: write to file
    if let Some(out_path) = params.get("output_path").and_then(|v| v.as_str()) {
        let path = expand_path(out_path, workspace);
        write_json_to_csv(&path, &data, ",")?;
    }

    Ok(result)
}

fn match_filter(val: Option<&Value>, condition: &Value) -> bool {
    match condition {
        // Direct equality: {"column": "value"}
        Value::String(s) => {
            val.map(|v| match v {
                Value::String(vs) => vs == s,
                _ => v.to_string().trim_matches('"') == s.as_str(),
            }).unwrap_or(false)
        }
        Value::Number(n) => {
            val.map(|v| v.as_f64() == n.as_f64()).unwrap_or(false)
        }
        Value::Bool(b) => {
            val.map(|v| v.as_bool() == Some(*b)).unwrap_or(false)
        }
        // Operator conditions: {"column": {"gt": 18, "contains": "foo"}}
        Value::Object(ops) => {
            ops.iter().all(|(op, target)| {
                match op.as_str() {
                    "eq" => val.map(|v| v == target).unwrap_or(false),
                    "ne" | "neq" => val.map(|v| v != target).unwrap_or(true),
                    "gt" => {
                        val.and_then(|v| v.as_f64())
                            .zip(target.as_f64())
                            .map(|(a, b)| a > b)
                            .unwrap_or(false)
                    }
                    "gte" | "ge" => {
                        val.and_then(|v| v.as_f64())
                            .zip(target.as_f64())
                            .map(|(a, b)| a >= b)
                            .unwrap_or(false)
                    }
                    "lt" => {
                        val.and_then(|v| v.as_f64())
                            .zip(target.as_f64())
                            .map(|(a, b)| a < b)
                            .unwrap_or(false)
                    }
                    "lte" | "le" => {
                        val.and_then(|v| v.as_f64())
                            .zip(target.as_f64())
                            .map(|(a, b)| a <= b)
                            .unwrap_or(false)
                    }
                    "contains" => {
                        let target_str = target.as_str().unwrap_or("");
                        val.map(|v| {
                            let s = match v {
                                Value::String(s) => s.clone(),
                                _ => v.to_string(),
                            };
                            s.contains(target_str)
                        }).unwrap_or(false)
                    }
                    "starts_with" => {
                        let target_str = target.as_str().unwrap_or("");
                        val.and_then(|v| v.as_str())
                            .map(|s| s.starts_with(target_str))
                            .unwrap_or(false)
                    }
                    "ends_with" => {
                        let target_str = target.as_str().unwrap_or("");
                        val.and_then(|v| v.as_str())
                            .map(|s| s.ends_with(target_str))
                            .unwrap_or(false)
                    }
                    "in" => {
                        if let Some(arr) = target.as_array() {
                            val.map(|v| arr.contains(v)).unwrap_or(false)
                        } else {
                            false
                        }
                    }
                    "is_null" => {
                        let expect_null = target.as_bool().unwrap_or(true);
                        let is_null = val.is_none() || val == Some(&Value::Null);
                        is_null == expect_null
                    }
                    _ => true, // Unknown operator, skip
                }
            })
        }
        Value::Null => val.is_none() || val == Some(&Value::Null),
        _ => false,
    }
}

fn compare_values(a: Option<&Value>, b: Option<&Value>) -> std::cmp::Ordering {
    match (a, b) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (Some(_), None) => std::cmp::Ordering::Greater,
        (Some(va), Some(vb)) => {
            // Try numeric comparison first
            if let (Some(fa), Some(fb)) = (va.as_f64(), vb.as_f64()) {
                return fa.partial_cmp(&fb).unwrap_or(std::cmp::Ordering::Equal);
            }
            // Fall back to string comparison
            let sa = match va {
                Value::String(s) => s.clone(),
                _ => va.to_string(),
            };
            let sb = match vb {
                Value::String(s) => s.clone(),
                _ => vb.to_string(),
            };
            sa.cmp(&sb)
        }
    }
}

fn action_stats(workspace: &PathBuf, params: &Value) -> Result<Value> {
    let data = load_data(workspace, params)?;

    if data.is_empty() {
        return Ok(json!({
            "total_rows": 0,
            "stats": {}
        }));
    }

    let group_by = params.get("group_by").and_then(|v| v.as_str());
    let agg_column = params.get("agg_column").and_then(|v| v.as_str());
    let agg_func = params.get("agg_func").and_then(|v| v.as_str()).unwrap_or("count");

    // Special case: correlation without group_by
    if agg_func == "correlation" {
        let col_a = agg_column.ok_or_else(|| Error::Tool("correlation requires 'agg_column'".into()))?;
        let col_b = params.get("correlation_column").and_then(|v| v.as_str())
            .ok_or_else(|| Error::Tool("correlation requires 'correlation_column'".into()))?;
        let corr = compute_correlation(&data, col_a, col_b);
        return Ok(json!({
            "total_rows": data.len(),
            "agg_func": "correlation",
            "column_a": col_a,
            "column_b": col_b,
            "correlation": corr,
            "interpretation": corr.map(|r| {
                if r.abs() > 0.8 { "strong" }
                else if r.abs() > 0.5 { "moderate" }
                else if r.abs() > 0.3 { "weak" }
                else { "negligible" }
            }).unwrap_or("insufficient_data")
        }));
    }

    // Special case: percentile without group_by
    if agg_func == "percentile" {
        let col = agg_column.ok_or_else(|| Error::Tool("percentile requires 'agg_column'".into()))?;
        let p_val = params.get("percentile_value").and_then(|v| v.as_f64()).unwrap_or(50.0);
        let mut values: Vec<f64> = data.iter()
            .filter_map(|row| row.get(col).and_then(|v| v.as_f64()))
            .collect();
        values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let result_val = if values.is_empty() { None } else { Some(compute_percentile_sorted(&values, p_val)) };
        return Ok(json!({
            "total_rows": data.len(),
            "agg_func": "percentile",
            "column": col,
            "percentile": p_val,
            "value": result_val
        }));
    }

    if let Some(group_col) = group_by {
        // Grouped aggregation
        let mut groups: std::collections::HashMap<String, Vec<&Value>> = std::collections::HashMap::new();
        for row in &data {
            let key = row.get(group_col)
                .map(|v| match v {
                    Value::String(s) => s.clone(),
                    _ => v.to_string(),
                })
                .unwrap_or_else(|| "(null)".to_string());
            groups.entry(key).or_default().push(row);
        }

        let percentile_val = params.get("percentile_value").and_then(|v| v.as_f64()).unwrap_or(50.0);

        let mut results = Vec::new();
        for (key, rows) in &groups {
            let agg_val = if let Some(col) = agg_column {
                match agg_func {
                    "percentile" => {
                        let mut vals: Vec<f64> = rows.iter()
                            .filter_map(|row| row.get(col).and_then(|v| v.as_f64()))
                            .collect();
                        vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                        if vals.is_empty() { json!(null) } else { json!(compute_percentile_sorted(&vals, percentile_val)) }
                    }
                    _ => compute_agg(rows, col, agg_func),
                }
            } else {
                json!(rows.len())
            };
            results.push(json!({
                group_col: key,
                format!("{}_{}", agg_func, agg_column.unwrap_or("*")): agg_val,
                "count": rows.len()
            }));
        }

        // Sort by group key
        results.sort_by(|a, b| {
            let ka = a.get(group_col).and_then(|v| v.as_str()).unwrap_or("");
            let kb = b.get(group_col).and_then(|v| v.as_str()).unwrap_or("");
            ka.cmp(kb)
        });

        let result = json!({
            "total_rows": data.len(),
            "groups": results.len(),
            "data": results
        });

        if let Some(out_path) = params.get("output_path").and_then(|v| v.as_str()) {
            let path = expand_path(out_path, workspace);
            write_json_to_csv(&path, &results, ",")?;
        }

        Ok(result)
    } else {
        // Overall statistics for all numeric columns
        let columns: Vec<String> = data.first()
            .and_then(|r| r.as_object())
            .map(|obj| obj.keys().cloned().collect())
            .unwrap_or_default();

        let mut col_stats = serde_json::Map::new();
        for col in &columns {
            let values: Vec<f64> = data.iter()
                .filter_map(|row| row.get(col).and_then(|v| v.as_f64()))
                .collect();

            if !values.is_empty() {
                let count = values.len();
                let sum: f64 = values.iter().sum();
                let avg = sum / count as f64;
                let min = values.iter().cloned().fold(f64::INFINITY, f64::min);
                let max = values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);

                // Standard deviation
                let variance = values.iter().map(|v| (v - avg).powi(2)).sum::<f64>() / count as f64;
                let std_dev = variance.sqrt();

                // Median
                let mut sorted = values.clone();
                sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                let median = if count % 2 == 0 {
                    (sorted[count / 2 - 1] + sorted[count / 2]) / 2.0
                } else {
                    sorted[count / 2]
                };

                // Percentiles
                let p25 = compute_percentile_sorted(&sorted, 25.0);
                let p75 = compute_percentile_sorted(&sorted, 75.0);
                let p90 = compute_percentile_sorted(&sorted, 90.0);

                col_stats.insert(col.clone(), json!({
                    "count": count,
                    "sum": sum,
                    "avg": (avg * 1000.0).round() / 1000.0,
                    "min": min,
                    "max": max,
                    "median": median,
                    "std_dev": (std_dev * 1000.0).round() / 1000.0,
                    "p25": p25,
                    "p75": p75,
                    "p90": p90
                }));
            } else {
                // Non-numeric column: count distinct values
                let distinct: std::collections::HashSet<String> = data.iter()
                    .filter_map(|row| row.get(col))
                    .map(|v| match v {
                        Value::String(s) => s.clone(),
                        Value::Null => "(null)".to_string(),
                        _ => v.to_string(),
                    })
                    .collect();
                let null_count = data.iter()
                    .filter(|row| row.get(col).is_none() || row.get(col) == Some(&Value::Null))
                    .count();

                col_stats.insert(col.clone(), json!({
                    "type": "categorical",
                    "count": data.len(),
                    "distinct": distinct.len(),
                    "null_count": null_count
                }));
            }
        }

        Ok(json!({
            "total_rows": data.len(),
            "columns": columns,
            "stats": col_stats
        }))
    }
}

fn compute_agg(rows: &[&Value], column: &str, func: &str) -> Value {
    let values: Vec<f64> = rows.iter()
        .filter_map(|row| row.get(column).and_then(|v| v.as_f64()))
        .collect();

    match func {
        "count" => json!(rows.len()),
        "sum" => json!(values.iter().sum::<f64>()),
        "avg" => {
            if values.is_empty() {
                json!(null)
            } else {
                let avg = values.iter().sum::<f64>() / values.len() as f64;
                json!((avg * 1000.0).round() / 1000.0)
            }
        }
        "min" => {
            values.iter().cloned().fold(None, |min: Option<f64>, v| {
                Some(min.map_or(v, |m: f64| m.min(v)))
            }).map(|v| json!(v)).unwrap_or(json!(null))
        }
        "max" => {
            values.iter().cloned().fold(None, |max: Option<f64>, v| {
                Some(max.map_or(v, |m: f64| m.max(v)))
            }).map(|v| json!(v)).unwrap_or(json!(null))
        }
        "distinct" => {
            let distinct: std::collections::HashSet<String> = rows.iter()
                .filter_map(|row| row.get(column))
                .map(|v| match v {
                    Value::String(s) => s.clone(),
                    _ => v.to_string(),
                })
                .collect();
            json!(distinct.len())
        }
        "median" => {
            if values.is_empty() {
                json!(null)
            } else {
                let mut sorted = values.clone();
                sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                let med = compute_percentile_sorted(&sorted, 50.0);
                json!(med)
            }
        }
        "percentile" => {
            json!(null) // percentile needs extra param, handled at caller level
        }
        "correlation" => {
            json!(null) // correlation needs extra param, handled at caller level
        }
        _ => json!(null),
    }
}

/// Compute percentile from a pre-sorted slice using linear interpolation.
fn compute_percentile_sorted(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let p = p.clamp(0.0, 100.0);
    let rank = (p / 100.0) * (sorted.len() - 1) as f64;
    let lower = rank.floor() as usize;
    let upper = rank.ceil() as usize;
    if lower == upper {
        sorted[lower]
    } else {
        let frac = rank - lower as f64;
        let result = sorted[lower] * (1.0 - frac) + sorted[upper] * frac;
        (result * 10000.0).round() / 10000.0
    }
}

/// Compute Pearson correlation coefficient between two columns.
fn compute_correlation(data: &[Value], col_a: &str, col_b: &str) -> Option<f64> {
    let pairs: Vec<(f64, f64)> = data.iter()
        .filter_map(|row| {
            let a = row.get(col_a).and_then(|v| v.as_f64())?;
            let b = row.get(col_b).and_then(|v| v.as_f64())?;
            Some((a, b))
        })
        .collect();

    if pairs.len() < 2 {
        return None;
    }

    let n = pairs.len() as f64;
    let sum_a: f64 = pairs.iter().map(|(a, _)| a).sum();
    let sum_b: f64 = pairs.iter().map(|(_, b)| b).sum();
    let mean_a = sum_a / n;
    let mean_b = sum_b / n;

    let mut cov = 0.0;
    let mut var_a = 0.0;
    let mut var_b = 0.0;
    for (a, b) in &pairs {
        let da = a - mean_a;
        let db = b - mean_b;
        cov += da * db;
        var_a += da * da;
        var_b += db * db;
    }

    let denom = (var_a * var_b).sqrt();
    if denom < 1e-15 {
        return None; // constant column
    }

    let r = cov / denom;
    Some((r * 10000.0).round() / 10000.0)
}

fn action_transform(workspace: &PathBuf, params: &Value) -> Result<Value> {
    let mut data = load_data(workspace, params)?;

    let ops = params.get("transform_ops")
        .and_then(|v| v.as_array())
        .ok_or_else(|| Error::Validation("transform requires 'transform_ops' array".to_string()))?;

    for op_def in ops {
        let op = op_def.get("op").and_then(|v| v.as_str()).unwrap_or("");
        match op {
            "rename" => {
                let from = op_def.get("from").and_then(|v| v.as_str()).unwrap_or("");
                let to = op_def.get("to").and_then(|v| v.as_str()).unwrap_or("");
                if !from.is_empty() && !to.is_empty() {
                    data = data.into_iter().map(|mut row| {
                        if let Some(obj) = row.as_object_mut() {
                            if let Some(val) = obj.remove(from) {
                                obj.insert(to.to_string(), val);
                            }
                        }
                        row
                    }).collect();
                }
            }
            "drop" => {
                if let Some(cols) = op_def.get("columns").and_then(|v| v.as_array()) {
                    let drop_cols: Vec<&str> = cols.iter().filter_map(|c| c.as_str()).collect();
                    data = data.into_iter().map(|mut row| {
                        if let Some(obj) = row.as_object_mut() {
                            for col in &drop_cols {
                                obj.remove(*col);
                            }
                        }
                        row
                    }).collect();
                }
            }
            "fill_null" => {
                let column = op_def.get("column").and_then(|v| v.as_str()).unwrap_or("");
                let fill_value = op_def.get("value").cloned().unwrap_or(Value::String(String::new()));
                if !column.is_empty() {
                    data = data.into_iter().map(|mut row| {
                        if let Some(obj) = row.as_object_mut() {
                            let is_null = obj.get(column).map(|v| v.is_null()).unwrap_or(true);
                            if is_null {
                                obj.insert(column.to_string(), fill_value.clone());
                            }
                        }
                        row
                    }).collect();
                }
            }
            "dedup" => {
                let cols = op_def.get("columns").and_then(|v| v.as_array());
                let mut seen = std::collections::HashSet::new();
                data = data.into_iter().filter(|row| {
                    let key = if let Some(cols) = cols {
                        let parts: Vec<String> = cols.iter()
                            .filter_map(|c| c.as_str())
                            .map(|c| row.get(c).map(|v| v.to_string()).unwrap_or_default())
                            .collect();
                        parts.join("|")
                    } else {
                        row.to_string()
                    };
                    seen.insert(key)
                }).collect();
            }
            "add_column" => {
                let name = op_def.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let value = op_def.get("value").cloned().unwrap_or(Value::Null);
                if !name.is_empty() {
                    data = data.into_iter().map(|mut row| {
                        if let Some(obj) = row.as_object_mut() {
                            obj.insert(name.to_string(), value.clone());
                        }
                        row
                    }).collect();
                }
            }
            "to_number" => {
                let column = op_def.get("column").and_then(|v| v.as_str()).unwrap_or("");
                if !column.is_empty() {
                    data = data.into_iter().map(|mut row| {
                        if let Some(obj) = row.as_object_mut() {
                            if let Some(val) = obj.get(column).cloned() {
                                let num = match &val {
                                    Value::String(s) => {
                                        let trimmed = s.trim().replace(',', "");
                                        if let Ok(i) = trimmed.parse::<i64>() {
                                            json!(i)
                                        } else if let Ok(f) = trimmed.parse::<f64>() {
                                            json!(f)
                                        } else {
                                            val
                                        }
                                    }
                                    _ => val,
                                };
                                obj.insert(column.to_string(), num);
                            }
                        }
                        row
                    }).collect();
                }
            }
            _ => {
                // Unknown op, skip
            }
        }
    }

    let result = json!({
        "total_rows": data.len(),
        "data": data
    });

    if let Some(out_path) = params.get("output_path").and_then(|v| v.as_str()) {
        let path = expand_path(out_path, workspace);
        write_json_to_csv(&path, &data, ",")?;
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema() {
        let tool = DataProcessTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "data_process");
    }

    #[test]
    fn test_validate() {
        let tool = DataProcessTool;
        assert!(tool.validate(&json!({"action": "read_csv", "path": "test.csv"})).is_ok());
        assert!(tool.validate(&json!({"action": "read_csv"})).is_err());
        assert!(tool.validate(&json!({"action": "write_csv", "path": "out.csv", "data": []})).is_ok());
    }

    #[test]
    fn test_try_parse_value() {
        assert_eq!(try_parse_value("42"), json!(42));
        assert_eq!(try_parse_value("3.14"), json!(3.14));
        assert_eq!(try_parse_value("true"), json!(true));
        assert_eq!(try_parse_value("hello"), json!("hello"));
        assert_eq!(try_parse_value(""), Value::Null);
    }

    #[test]
    fn test_match_filter() {
        let val = Some(&json!("active"));
        assert!(match_filter(val, &json!("active")));
        assert!(!match_filter(val, &json!("inactive")));

        let num_val = Some(&json!(25));
        assert!(match_filter(num_val, &json!({"gt": 18})));
        assert!(!match_filter(num_val, &json!({"gt": 30})));
        assert!(match_filter(num_val, &json!({"gte": 25})));

        let str_val = Some(&json!("John Smith"));
        assert!(match_filter(str_val, &json!({"contains": "John"})));
        assert!(!match_filter(str_val, &json!({"contains": "Jane"})));
    }

    #[test]
    fn test_compare_values() {
        assert_eq!(compare_values(Some(&json!(1)), Some(&json!(2))), std::cmp::Ordering::Less);
        assert_eq!(compare_values(Some(&json!(2)), Some(&json!(1))), std::cmp::Ordering::Greater);
        assert_eq!(compare_values(Some(&json!("a")), Some(&json!("b"))), std::cmp::Ordering::Less);
    }

    #[test]
    fn test_compute_percentile_sorted() {
        let data = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0];
        assert_eq!(compute_percentile_sorted(&data, 0.0), 1.0);
        assert_eq!(compute_percentile_sorted(&data, 100.0), 10.0);
        assert_eq!(compute_percentile_sorted(&data, 50.0), 5.5);
        // P25 = 1 + 0.25*9 = 3.25
        assert_eq!(compute_percentile_sorted(&data, 25.0), 3.25);
        // P75 = 1 + 0.75*9 = 7.75
        assert_eq!(compute_percentile_sorted(&data, 75.0), 7.75);

        // Edge cases
        assert_eq!(compute_percentile_sorted(&[], 50.0), 0.0);
        assert_eq!(compute_percentile_sorted(&[42.0], 50.0), 42.0);
    }

    #[test]
    fn test_compute_correlation() {
        // Perfect positive correlation
        let data = vec![
            json!({"x": 1, "y": 2}),
            json!({"x": 2, "y": 4}),
            json!({"x": 3, "y": 6}),
            json!({"x": 4, "y": 8}),
            json!({"x": 5, "y": 10}),
        ];
        let corr = compute_correlation(&data, "x", "y").unwrap();
        assert_eq!(corr, 1.0);

        // Perfect negative correlation
        let data_neg = vec![
            json!({"a": 1, "b": 10}),
            json!({"a": 2, "b": 8}),
            json!({"a": 3, "b": 6}),
            json!({"a": 4, "b": 4}),
            json!({"a": 5, "b": 2}),
        ];
        let corr_neg = compute_correlation(&data_neg, "a", "b").unwrap();
        assert_eq!(corr_neg, -1.0);

        // Insufficient data
        let data_one = vec![json!({"x": 1, "y": 2})];
        assert!(compute_correlation(&data_one, "x", "y").is_none());

        // Missing column
        assert!(compute_correlation(&data, "x", "z").is_none());
    }
}
