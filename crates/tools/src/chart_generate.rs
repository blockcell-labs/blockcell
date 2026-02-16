use async_trait::async_trait;
use blockcell_core::{Error, Result};
use serde_json::{json, Value};
use tracing::info;

use crate::{Tool, ToolContext, ToolSchema};

/// Tool for generating charts and data visualizations.
///
/// Generates Python scripts using matplotlib/plotly and executes them to produce
/// chart images (PNG/SVG) or interactive HTML files.
pub struct ChartGenerateTool;

#[async_trait]
impl Tool for ChartGenerateTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "chart_generate",
            description: "Generate charts and data visualizations. Creates bar, line, pie, scatter, histogram, heatmap charts as PNG/SVG images or interactive HTML. Uses matplotlib or plotly via Python.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["generate", "info"],
                        "description": "Action: 'generate' creates a chart, 'info' checks available backends"
                    },
                    "chart_type": {
                        "type": "string",
                        "enum": ["bar", "line", "pie", "scatter", "histogram", "heatmap", "area", "box", "custom"],
                        "description": "Type of chart to generate"
                    },
                    "data": {
                        "type": "object",
                        "description": "Chart data. Format depends on chart_type. Common: {labels: [...], values: [...]} or {x: [...], y: [...]} or {series: [{name, values}, ...]}"
                    },
                    "title": {
                        "type": "string",
                        "description": "Chart title"
                    },
                    "x_label": {
                        "type": "string",
                        "description": "X-axis label"
                    },
                    "y_label": {
                        "type": "string",
                        "description": "Y-axis label"
                    },
                    "output_path": {
                        "type": "string",
                        "description": "Output file path. Supports .png, .svg, .html. Default: auto-generated PNG in workspace"
                    },
                    "style": {
                        "type": "object",
                        "description": "Style options: {width, height, colors, theme, font_size, legend, grid}"
                    },
                    "backend": {
                        "type": "string",
                        "enum": ["auto", "matplotlib", "plotly"],
                        "description": "Rendering backend. Default: 'auto' (matplotlib for images, plotly for HTML)"
                    },
                    "custom_script": {
                        "type": "string",
                        "description": "Custom Python script for 'custom' chart_type. Must save output to OUTPUT_PATH variable."
                    }
                },
                "required": ["action"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("");
        if !["generate", "info"].contains(&action) {
            return Err(Error::Tool("action must be 'generate' or 'info'".into()));
        }
        if action == "generate" {
            let chart_type = params.get("chart_type").and_then(|v| v.as_str()).unwrap_or("");
            if chart_type.is_empty() {
                return Err(Error::Tool("'chart_type' is required for generate".into()));
            }
            if chart_type != "custom" && params.get("data").is_none() {
                return Err(Error::Tool("'data' is required for generate (unless chart_type is 'custom')".into()));
            }
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("info");

        match action {
            "generate" => action_generate(&ctx, &params).await,
            "info" => action_info().await,
            _ => Err(Error::Tool(format!("Unknown action: {}", action))),
        }
    }
}

/// Check available chart generation backends.
async fn action_info() -> Result<Value> {
    let has_python = which::which("python3").is_ok() || which::which("python").is_ok();
    let python_bin = if which::which("python3").is_ok() { "python3" } else { "python" };

    let mut has_matplotlib = false;
    let mut has_plotly = false;

    if has_python {
        // Check matplotlib
        if let Ok(output) = tokio::process::Command::new(python_bin)
            .args(["-c", "import matplotlib; print(matplotlib.__version__)"])
            .output()
            .await
        {
            has_matplotlib = output.status.success();
        }

        // Check plotly
        if let Ok(output) = tokio::process::Command::new(python_bin)
            .args(["-c", "import plotly; print(plotly.__version__)"])
            .output()
            .await
        {
            has_plotly = output.status.success();
        }
    }

    let install_hint = if !has_matplotlib && !has_plotly {
        "Install: pip install matplotlib plotly"
    } else if !has_matplotlib {
        "Install matplotlib: pip install matplotlib"
    } else if !has_plotly {
        "Optional: pip install plotly (for interactive HTML charts)"
    } else {
        ""
    };

    Ok(json!({
        "has_python": has_python,
        "python_bin": python_bin,
        "has_matplotlib": has_matplotlib,
        "has_plotly": has_plotly,
        "supported_chart_types": ["bar", "line", "pie", "scatter", "histogram", "heatmap", "area", "box", "custom"],
        "supported_output_formats": ["png", "svg", "html"],
        "install_hint": install_hint,
    }))
}

/// Generate a chart.
async fn action_generate(ctx: &ToolContext, params: &Value) -> Result<Value> {
    let chart_type = params.get("chart_type").and_then(|v| v.as_str()).unwrap_or("bar");
    let data = params.get("data").cloned().unwrap_or(json!({}));
    let title = params.get("title").and_then(|v| v.as_str()).unwrap_or("");
    let x_label = params.get("x_label").and_then(|v| v.as_str()).unwrap_or("");
    let y_label = params.get("y_label").and_then(|v| v.as_str()).unwrap_or("");
    let style = params.get("style").cloned().unwrap_or(json!({}));
    let backend = params.get("backend").and_then(|v| v.as_str()).unwrap_or("auto");
    let custom_script = params.get("custom_script").and_then(|v| v.as_str());

    // Determine output path and format
    let output_path = if let Some(op) = params.get("output_path").and_then(|v| v.as_str()) {
        op.to_string()
    } else {
        let dir = ctx.workspace.join("charts");
        let _ = std::fs::create_dir_all(&dir);
        let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
        dir.join(format!("chart_{}_{}.png", chart_type, timestamp))
            .to_string_lossy().to_string()
    };

    if let Some(parent) = std::path::Path::new(&output_path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let output_ext = std::path::Path::new(&output_path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("png")
        .to_lowercase();

    // Determine backend
    let use_plotly = match backend {
        "plotly" => true,
        "matplotlib" => false,
        _ => output_ext == "html",
    };

    // Generate Python script
    let script = if chart_type == "custom" {
        if let Some(cs) = custom_script {
            format!(
                "import os\nOUTPUT_PATH = {}\n{}",
                quote_python_str(&output_path),
                cs
            )
        } else {
            return Err(Error::Tool("'custom_script' is required for chart_type='custom'".into()));
        }
    } else if use_plotly {
        generate_plotly_script(chart_type, &data, title, x_label, y_label, &style, &output_path)?
    } else {
        generate_matplotlib_script(chart_type, &data, title, x_label, y_label, &style, &output_path, &output_ext)?
    };

    // Find python binary
    let python_bin = if which::which("python3").is_ok() { "python3" } else { "python" };
    if which::which(python_bin).is_err() {
        return Err(Error::Tool("Python not found. Install Python 3 to generate charts.".into()));
    }

    // Write script to temp file
    let script_dir = ctx.workspace.join("tmp");
    let _ = std::fs::create_dir_all(&script_dir);
    let script_path = script_dir.join("_chart_gen.py");
    std::fs::write(&script_path, &script)
        .map_err(|e| Error::Tool(format!("Failed to write chart script: {}", e)))?;

    info!(chart_type = %chart_type, output = %output_path, backend = if use_plotly { "plotly" } else { "matplotlib" }, "📊 Generating chart");

    // Execute
    let output = tokio::process::Command::new(python_bin)
        .arg(&script_path)
        .output()
        .await
        .map_err(|e| Error::Tool(format!("Python execution failed: {}", e)))?;

    // Clean up script
    let _ = std::fs::remove_file(&script_path);

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Tool(format!(
            "Chart generation failed: {}\nScript:\n{}",
            truncate_str(&stderr, 500),
            truncate_str(&script, 300)
        )));
    }

    // Verify output exists
    if !std::path::Path::new(&output_path).exists() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(Error::Tool(format!(
            "Chart file not created at {}. stdout: {}",
            output_path,
            truncate_str(&stdout, 300)
        )));
    }

    let file_size = std::fs::metadata(&output_path).map(|m| m.len()).unwrap_or(0);

    info!(path = %output_path, size = file_size, "📊 Chart generated");

    Ok(json!({
        "success": true,
        "chart_type": chart_type,
        "output_path": output_path,
        "format": output_ext,
        "backend": if use_plotly { "plotly" } else { "matplotlib" },
        "file_size_bytes": file_size,
    }))
}

/// Generate a matplotlib Python script.
fn generate_matplotlib_script(
    chart_type: &str,
    data: &Value,
    title: &str,
    x_label: &str,
    y_label: &str,
    style: &Value,
    output_path: &str,
    _output_ext: &str,
) -> Result<String> {
    let width = style.get("width").and_then(|v| v.as_f64()).unwrap_or(10.0);
    let height = style.get("height").and_then(|v| v.as_f64()).unwrap_or(6.0);
    let font_size = style.get("font_size").and_then(|v| v.as_u64()).unwrap_or(12);
    let grid = style.get("grid").and_then(|v| v.as_bool()).unwrap_or(true);
    let theme = style.get("theme").and_then(|v| v.as_str()).unwrap_or("seaborn-v0_8-whitegrid");
    let colors_json = style.get("colors").cloned().unwrap_or(json!(null));

    let data_json = serde_json::to_string(data)
        .map_err(|e| Error::Tool(format!("Failed to serialize data: {}", e)))?;

    let colors_setup = if let Some(arr) = colors_json.as_array() {
        let c: Vec<String> = arr.iter().filter_map(|v| v.as_str().map(|s| format!("'{}'", s))).collect();
        format!("colors = [{}]", c.join(", "))
    } else {
        "colors = None".to_string()
    };

    let chart_code = match chart_type {
        "bar" => r#"
labels = data.get('labels', data.get('x', []))
values = data.get('values', data.get('y', []))
series = data.get('series', None)
if series:
    x = range(len(labels))
    w = 0.8 / len(series)
    for i, s in enumerate(series):
        offset = (i - len(series)/2 + 0.5) * w
        c = colors[i % len(colors)] if colors else None
        ax.bar([xi + offset for xi in x], s.get('values', s.get('y', [])), w, label=s.get('name',''), color=c)
    ax.set_xticks(range(len(labels)))
    ax.set_xticklabels(labels)
    ax.legend()
else:
    ax.bar(labels, values, color=colors)
"#,
        "line" => r#"
series = data.get('series', None)
if series:
    for i, s in enumerate(series):
        x = s.get('x', list(range(len(s.get('values', s.get('y', []))))))
        y = s.get('values', s.get('y', []))
        c = colors[i % len(colors)] if colors else None
        ax.plot(x, y, label=s.get('name',''), color=c, marker='o', markersize=4)
    ax.legend()
else:
    x = data.get('x', data.get('labels', list(range(len(data.get('y', data.get('values', [])))))))
    y = data.get('y', data.get('values', []))
    ax.plot(x, y, marker='o', markersize=4, color=colors[0] if colors else None)
"#,
        "pie" => r#"
labels = data.get('labels', [])
values = data.get('values', [])
ax.pie(values, labels=labels, autopct='%1.1f%%', colors=colors)
ax.axis('equal')
"#,
        "scatter" => r#"
series = data.get('series', None)
if series:
    for i, s in enumerate(series):
        c = colors[i % len(colors)] if colors else None
        ax.scatter(s.get('x', []), s.get('y', []), label=s.get('name',''), color=c, alpha=0.7)
    ax.legend()
else:
    ax.scatter(data.get('x', []), data.get('y', []), color=colors[0] if colors else None, alpha=0.7)
"#,
        "histogram" => r#"
values = data.get('values', data.get('x', []))
bins = data.get('bins', 'auto')
ax.hist(values, bins=bins, color=colors[0] if colors else None, edgecolor='white', alpha=0.8)
"#,
        "heatmap" => r#"
import numpy as np
matrix = np.array(data.get('matrix', data.get('values', [[]])))
labels_x = data.get('x_labels', data.get('columns', None))
labels_y = data.get('y_labels', data.get('rows', None))
im = ax.imshow(matrix, cmap='YlOrRd', aspect='auto')
fig.colorbar(im, ax=ax)
if labels_x:
    ax.set_xticks(range(len(labels_x)))
    ax.set_xticklabels(labels_x, rotation=45, ha='right')
if labels_y:
    ax.set_yticks(range(len(labels_y)))
    ax.set_yticklabels(labels_y)
"#,
        "area" => r#"
series = data.get('series', None)
if series:
    for i, s in enumerate(series):
        x = s.get('x', list(range(len(s.get('values', s.get('y', []))))))
        y = s.get('values', s.get('y', []))
        c = colors[i % len(colors)] if colors else None
        ax.fill_between(x, y, alpha=0.4, label=s.get('name',''), color=c)
        ax.plot(x, y, color=c)
    ax.legend()
else:
    x = data.get('x', list(range(len(data.get('y', data.get('values', []))))))
    y = data.get('y', data.get('values', []))
    ax.fill_between(x, y, alpha=0.4, color=colors[0] if colors else None)
"#,
        "box" => r#"
datasets = data.get('datasets', [data.get('values', [])])
labels = data.get('labels', [f'Group {i+1}' for i in range(len(datasets))])
bp = ax.boxplot(datasets, labels=labels, patch_artist=True)
if colors:
    for i, patch in enumerate(bp['boxes']):
        patch.set_facecolor(colors[i % len(colors)])
"#,
        _ => return Err(Error::Tool(format!("Unsupported chart_type for matplotlib: {}", chart_type))),
    };

    Ok(format!(
        r#"import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt
import json

try:
    plt.style.use('{theme}')
except:
    pass

plt.rcParams.update({{'font.size': {font_size}}})

data = json.loads('''{data_json}''')
{colors_setup}

fig, ax = plt.subplots(figsize=({width}, {height}))

{chart_code}

if '{title}':
    ax.set_title('{title}', fontsize={font_size}+2, fontweight='bold')
if '{x_label}':
    ax.set_xlabel('{x_label}')
if '{y_label}':
    ax.set_ylabel('{y_label}')
if {grid}:
    ax.grid(True, alpha=0.3)

plt.tight_layout()
plt.savefig({output_path}, dpi=150, bbox_inches='tight')
plt.close()
print('OK')
"#,
        theme = theme,
        font_size = font_size,
        data_json = data_json.replace('\\', "\\\\").replace('\'', "\\'"),
        colors_setup = colors_setup,
        width = width,
        height = height,
        chart_code = chart_code,
        title = title.replace('\'', "\\'"),
        x_label = x_label.replace('\'', "\\'"),
        y_label = y_label.replace('\'', "\\'"),
        grid = if grid { "True" } else { "False" },
        output_path = quote_python_str(output_path),
    ))
}

/// Generate a plotly Python script (for interactive HTML output).
fn generate_plotly_script(
    chart_type: &str,
    data: &Value,
    title: &str,
    x_label: &str,
    y_label: &str,
    style: &Value,
    output_path: &str,
) -> Result<String> {
    let width = style.get("width").and_then(|v| v.as_u64()).unwrap_or(900);
    let height = style.get("height").and_then(|v| v.as_u64()).unwrap_or(500);
    let colors_json = style.get("colors").cloned().unwrap_or(json!(null));

    let data_json = serde_json::to_string(data)
        .map_err(|e| Error::Tool(format!("Failed to serialize data: {}", e)))?;

    let colors_setup = if let Some(arr) = colors_json.as_array() {
        let c: Vec<String> = arr.iter().filter_map(|v| v.as_str().map(|s| format!("'{}'", s))).collect();
        format!("colors = [{}]", c.join(", "))
    } else {
        "colors = None".to_string()
    };

    let trace_code = match chart_type {
        "bar" => r#"
series = data.get('series', None)
if series:
    for i, s in enumerate(series):
        c = colors[i % len(colors)] if colors else None
        fig.add_trace(go.Bar(x=data.get('labels', data.get('x', [])), y=s.get('values', s.get('y', [])), name=s.get('name',''), marker_color=c))
else:
    fig.add_trace(go.Bar(x=data.get('labels', data.get('x', [])), y=data.get('values', data.get('y', [])), marker_color=colors))
"#,
        "line" => r#"
series = data.get('series', None)
if series:
    for i, s in enumerate(series):
        x = s.get('x', list(range(len(s.get('values', s.get('y', []))))))
        y = s.get('values', s.get('y', []))
        c = colors[i % len(colors)] if colors else None
        fig.add_trace(go.Scatter(x=x, y=y, mode='lines+markers', name=s.get('name',''), line=dict(color=c)))
else:
    x = data.get('x', data.get('labels', list(range(len(data.get('y', data.get('values', [])))))))
    y = data.get('y', data.get('values', []))
    fig.add_trace(go.Scatter(x=x, y=y, mode='lines+markers'))
"#,
        "pie" => r#"
fig.add_trace(go.Pie(labels=data.get('labels', []), values=data.get('values', []), marker=dict(colors=colors) if colors else {}))
"#,
        "scatter" => r#"
series = data.get('series', None)
if series:
    for i, s in enumerate(series):
        c = colors[i % len(colors)] if colors else None
        fig.add_trace(go.Scatter(x=s.get('x', []), y=s.get('y', []), mode='markers', name=s.get('name',''), marker=dict(color=c, opacity=0.7)))
else:
    fig.add_trace(go.Scatter(x=data.get('x', []), y=data.get('y', []), mode='markers', marker=dict(opacity=0.7)))
"#,
        "histogram" => r#"
fig.add_trace(go.Histogram(x=data.get('values', data.get('x', [])), marker_color=colors[0] if colors else None))
"#,
        "heatmap" => r#"
fig.add_trace(go.Heatmap(
    z=data.get('matrix', data.get('values', [[]])),
    x=data.get('x_labels', data.get('columns', None)),
    y=data.get('y_labels', data.get('rows', None)),
    colorscale='YlOrRd'
))
"#,
        "area" => r#"
series = data.get('series', None)
if series:
    for i, s in enumerate(series):
        x = s.get('x', list(range(len(s.get('values', s.get('y', []))))))
        y = s.get('values', s.get('y', []))
        c = colors[i % len(colors)] if colors else None
        fig.add_trace(go.Scatter(x=x, y=y, fill='tozeroy', name=s.get('name',''), line=dict(color=c)))
else:
    x = data.get('x', list(range(len(data.get('y', data.get('values', []))))))
    y = data.get('y', data.get('values', []))
    fig.add_trace(go.Scatter(x=x, y=y, fill='tozeroy'))
"#,
        "box" => r#"
datasets = data.get('datasets', [data.get('values', [])])
labels = data.get('labels', [f'Group {i+1}' for i in range(len(datasets))])
for i, (ds, lbl) in enumerate(zip(datasets, labels)):
    c = colors[i % len(colors)] if colors else None
    fig.add_trace(go.Box(y=ds, name=lbl, marker_color=c))
"#,
        _ => return Err(Error::Tool(format!("Unsupported chart_type for plotly: {}", chart_type))),
    };

    Ok(format!(
        r#"import plotly.graph_objects as go
import json

data = json.loads('''{data_json}''')
{colors_setup}

fig = go.Figure()

{trace_code}

fig.update_layout(
    title={title},
    xaxis_title={x_label},
    yaxis_title={y_label},
    width={width},
    height={height},
    template='plotly_white',
)

output_path = {output_path}
if output_path.endswith('.html'):
    fig.write_html(output_path)
elif output_path.endswith('.svg'):
    fig.write_image(output_path, format='svg')
else:
    fig.write_image(output_path, format='png', scale=2)
print('OK')
"#,
        data_json = data_json.replace('\\', "\\\\").replace('\'', "\\'"),
        colors_setup = colors_setup,
        trace_code = trace_code,
        title = quote_python_str(title),
        x_label = quote_python_str(x_label),
        y_label = quote_python_str(y_label),
        width = width,
        height = height,
        output_path = quote_python_str(output_path),
    ))
}

fn quote_python_str(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{}...", truncated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema() {
        let tool = ChartGenerateTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "chart_generate");
    }

    #[test]
    fn test_validate_generate() {
        let tool = ChartGenerateTool;
        assert!(tool.validate(&json!({
            "action": "generate",
            "chart_type": "bar",
            "data": {"labels": ["A", "B"], "values": [1, 2]}
        })).is_ok());

        // Missing chart_type
        assert!(tool.validate(&json!({"action": "generate"})).is_err());

        // Missing data (non-custom)
        assert!(tool.validate(&json!({"action": "generate", "chart_type": "bar"})).is_err());

        // Custom without data is ok
        assert!(tool.validate(&json!({"action": "generate", "chart_type": "custom"})).is_ok());
    }

    #[test]
    fn test_validate_info() {
        let tool = ChartGenerateTool;
        assert!(tool.validate(&json!({"action": "info"})).is_ok());
        assert!(tool.validate(&json!({"action": "invalid"})).is_err());
    }

    #[test]
    fn test_quote_python_str() {
        assert_eq!(quote_python_str("hello"), "'hello'");
        assert_eq!(quote_python_str("it's"), "'it\\'s'");
        assert_eq!(quote_python_str("a\\b"), "'a\\\\b'");
    }

    #[test]
    fn test_generate_matplotlib_script() {
        let data = json!({"labels": ["A", "B", "C"], "values": [10, 20, 30]});
        let style = json!({});
        let result = generate_matplotlib_script("bar", &data, "Test", "X", "Y", &style, "/tmp/test.png", "png");
        assert!(result.is_ok());
        let script = result.unwrap();
        assert!(script.contains("matplotlib"));
        assert!(script.contains("bar"));
    }

    #[test]
    fn test_generate_plotly_script() {
        let data = json!({"labels": ["A", "B"], "values": [1, 2]});
        let style = json!({});
        let result = generate_plotly_script("pie", &data, "Pie Chart", "", "", &style, "/tmp/test.html");
        assert!(result.is_ok());
        let script = result.unwrap();
        assert!(script.contains("plotly"));
        assert!(script.contains("Pie"));
    }
}
