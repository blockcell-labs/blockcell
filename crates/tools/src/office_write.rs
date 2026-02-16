use async_trait::async_trait;
use blockcell_core::{Error, Result};
use serde_json::{json, Value};
use tracing::info;

use crate::{Tool, ToolContext, ToolSchema};

/// Tool for generating Office documents (PPTX, DOCX, XLSX).
///
/// Uses Python libraries (python-pptx, python-docx, openpyxl) to create
/// properly formatted Office files from structured data.
pub struct OfficeWriteTool;

#[async_trait]
impl Tool for OfficeWriteTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "office_write",
            description: "Generate Office documents: PPTX presentations, DOCX documents, XLSX spreadsheets. Creates properly formatted files from structured data using Python.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["create_pptx", "create_docx", "create_xlsx", "info"],
                        "description": "Action: create_pptx/create_docx/create_xlsx to generate files, 'info' to check backends"
                    },
                    "output_path": {
                        "type": "string",
                        "description": "Output file path. Default: auto-generated in workspace"
                    },
                    "title": {
                        "type": "string",
                        "description": "Document title"
                    },
                    "slides": {
                        "type": "array",
                        "description": "For PPTX: array of slides. Each slide: {layout, title, content, bullets, image_path, notes, table}. layout: 'title'|'content'|'two_content'|'blank'|'section'"
                    },
                    "sections": {
                        "type": "array",
                        "description": "For DOCX: array of sections. Each: {heading, level, content, bullets, table, image_path, page_break}"
                    },
                    "sheets": {
                        "type": "array",
                        "description": "For XLSX: array of sheets. Each: {name, headers, rows, column_widths, bold_header}"
                    },
                    "style": {
                        "type": "object",
                        "description": "Style options: {font, font_size, theme_color, author}"
                    }
                },
                "required": ["action"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("");
        if !["create_pptx", "create_docx", "create_xlsx", "info"].contains(&action) {
            return Err(Error::Tool("action must be 'create_pptx', 'create_docx', 'create_xlsx', or 'info'".into()));
        }
        match action {
            "create_pptx" => {
                if params.get("slides").and_then(|v| v.as_array()).map(|a| a.is_empty()).unwrap_or(true) {
                    return Err(Error::Tool("'slides' array is required for create_pptx".into()));
                }
            }
            "create_docx" => {
                if params.get("sections").and_then(|v| v.as_array()).map(|a| a.is_empty()).unwrap_or(true) {
                    return Err(Error::Tool("'sections' array is required for create_docx".into()));
                }
            }
            "create_xlsx" => {
                if params.get("sheets").and_then(|v| v.as_array()).map(|a| a.is_empty()).unwrap_or(true) {
                    return Err(Error::Tool("'sheets' array is required for create_xlsx".into()));
                }
            }
            _ => {}
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("info");

        match action {
            "create_pptx" => action_create_pptx(&ctx, &params).await,
            "create_docx" => action_create_docx(&ctx, &params).await,
            "create_xlsx" => action_create_xlsx(&ctx, &params).await,
            "info" => action_info().await,
            _ => Err(Error::Tool(format!("Unknown action: {}", action))),
        }
    }
}

/// Check available Python libraries for Office generation.
async fn action_info() -> Result<Value> {
    let has_python = which::which("python3").is_ok() || which::which("python").is_ok();
    let python_bin = if which::which("python3").is_ok() { "python3" } else { "python" };

    let mut libs = json!({});

    if has_python {
        for (pkg, import) in &[
            ("python-pptx", "pptx"),
            ("python-docx", "docx"),
            ("openpyxl", "openpyxl"),
        ] {
            let check = tokio::process::Command::new(python_bin)
                .args(["-c", &format!("import {}; print({}.__version__)", import, import)])
                .output()
                .await;
            let available = check.map(|o| o.status.success()).unwrap_or(false);
            libs[*pkg] = json!(available);
        }
    }

    let all_installed = libs.get("python-pptx").and_then(|v| v.as_bool()).unwrap_or(false)
        && libs.get("python-docx").and_then(|v| v.as_bool()).unwrap_or(false)
        && libs.get("openpyxl").and_then(|v| v.as_bool()).unwrap_or(false);

    let install_hint = if !all_installed {
        "Install: pip install python-pptx python-docx openpyxl"
    } else {
        ""
    };

    Ok(json!({
        "has_python": has_python,
        "libraries": libs,
        "all_installed": all_installed,
        "supported_formats": ["pptx", "docx", "xlsx"],
        "install_hint": install_hint,
    }))
}

/// Create a PPTX presentation.
async fn action_create_pptx(ctx: &ToolContext, params: &Value) -> Result<Value> {
    let slides = params.get("slides").and_then(|v| v.as_array())
        .ok_or_else(|| Error::Tool("'slides' is required".into()))?;
    let title = params.get("title").and_then(|v| v.as_str()).unwrap_or("Presentation");
    let style = params.get("style").cloned().unwrap_or(json!({}));

    let output_path = resolve_output_path(params, ctx, "pptx", "presentation");

    let slides_json = serde_json::to_string(slides)
        .map_err(|e| Error::Tool(format!("Failed to serialize slides: {}", e)))?;
    let style_json = serde_json::to_string(&style)
        .map_err(|e| Error::Tool(format!("Failed to serialize style: {}", e)))?;

    let script = format!(
        r#"
import json
from pptx import Presentation
from pptx.util import Inches, Pt, Emu
from pptx.enum.text import PP_ALIGN
from pptx.dml.color import RGBColor

slides_data = json.loads('''{slides_json}''')
style = json.loads('''{style_json}''')
title_text = {title}
output_path = {output_path}

prs = Presentation()
prs.slide_width = Inches(13.333)
prs.slide_height = Inches(7.5)

font_name = style.get('font', 'Calibri')
font_size = style.get('font_size', 18)

for i, slide_data in enumerate(slides_data):
    layout_name = slide_data.get('layout', 'content')
    slide_title = slide_data.get('title', '')
    content = slide_data.get('content', '')
    bullets = slide_data.get('bullets', [])
    notes = slide_data.get('notes', '')
    image_path = slide_data.get('image_path', '')
    table_data = slide_data.get('table', None)

    if layout_name == 'title':
        layout = prs.slide_layouts[0]
    elif layout_name == 'section':
        layout = prs.slide_layouts[2]
    elif layout_name == 'blank':
        layout = prs.slide_layouts[6]
    elif layout_name == 'two_content':
        layout = prs.slide_layouts[3]
    else:
        layout = prs.slide_layouts[1]

    slide = prs.slides.add_slide(layout)

    # Set title
    if slide_title and slide.shapes.title:
        slide.shapes.title.text = slide_title
        for para in slide.shapes.title.text_frame.paragraphs:
            for run in para.runs:
                run.font.name = font_name

    # Find content placeholder
    content_ph = None
    for ph in slide.placeholders:
        if ph.placeholder_format.idx == 1:
            content_ph = ph
            break

    # Add bullets or content
    if bullets and content_ph:
        tf = content_ph.text_frame
        tf.clear()
        for j, bullet in enumerate(bullets):
            if j == 0:
                para = tf.paragraphs[0]
            else:
                para = tf.add_paragraph()
            para.text = str(bullet)
            para.font.name = font_name
            para.font.size = Pt(font_size)
            para.level = 0
    elif content and content_ph:
        tf = content_ph.text_frame
        tf.clear()
        para = tf.paragraphs[0]
        para.text = content
        para.font.name = font_name
        para.font.size = Pt(font_size)

    # Add table
    if table_data:
        headers = table_data.get('headers', [])
        rows = table_data.get('rows', [])
        if headers and rows:
            num_rows = len(rows) + 1
            num_cols = len(headers)
            left = Inches(1)
            top = Inches(3.5) if slide_title else Inches(1.5)
            width = Inches(11)
            height = Inches(0.5 * num_rows)
            table_shape = slide.shapes.add_table(num_rows, num_cols, left, top, width, height)
            table = table_shape.table
            for ci, h in enumerate(headers):
                cell = table.cell(0, ci)
                cell.text = str(h)
                for para in cell.text_frame.paragraphs:
                    para.font.bold = True
                    para.font.name = font_name
                    para.font.size = Pt(font_size - 2)
            for ri, row in enumerate(rows):
                for ci, val in enumerate(row):
                    cell = table.cell(ri + 1, ci)
                    cell.text = str(val)
                    for para in cell.text_frame.paragraphs:
                        para.font.name = font_name
                        para.font.size = Pt(font_size - 2)

    # Add image
    if image_path:
        try:
            import os
            if os.path.exists(image_path):
                left = Inches(1)
                top = Inches(3) if slide_title else Inches(1)
                slide.shapes.add_picture(image_path, left, top, height=Inches(4))
        except Exception as e:
            pass

    # Add notes
    if notes:
        slide.notes_slide.notes_text_frame.text = notes

prs.save(output_path)
print('OK')
"#,
        slides_json = slides_json.replace('\\', "\\\\").replace('\'', "\\'"),
        style_json = style_json.replace('\\', "\\\\").replace('\'', "\\'"),
        title = quote_python_str(title),
        output_path = quote_python_str(&output_path),
    );

    run_python_script(&script, &output_path, "pptx", slides.len(), ctx).await
}

/// Create a DOCX document.
async fn action_create_docx(ctx: &ToolContext, params: &Value) -> Result<Value> {
    let sections = params.get("sections").and_then(|v| v.as_array())
        .ok_or_else(|| Error::Tool("'sections' is required".into()))?;
    let title = params.get("title").and_then(|v| v.as_str()).unwrap_or("");
    let style = params.get("style").cloned().unwrap_or(json!({}));

    let output_path = resolve_output_path(params, ctx, "docx", "document");

    let sections_json = serde_json::to_string(sections)
        .map_err(|e| Error::Tool(format!("Failed to serialize sections: {}", e)))?;
    let style_json = serde_json::to_string(&style)
        .map_err(|e| Error::Tool(format!("Failed to serialize style: {}", e)))?;

    let script = format!(
        r#"
import json
from docx import Document
from docx.shared import Pt, Inches
from docx.enum.text import WD_ALIGN_PARAGRAPH

sections_data = json.loads('''{sections_json}''')
style = json.loads('''{style_json}''')
title_text = {title}
output_path = {output_path}

doc = Document()

font_name = style.get('font', 'Calibri')
font_size = style.get('font_size', 11)
author = style.get('author', '')

# Set default font
doc_style = doc.styles['Normal']
doc_style.font.name = font_name
doc_style.font.size = Pt(font_size)

# Set author
if author:
    doc.core_properties.author = author

# Add title if provided
if title_text:
    doc.add_heading(title_text, level=0)

for section in sections_data:
    heading = section.get('heading', '')
    level = section.get('level', 1)
    content = section.get('content', '')
    bullets = section.get('bullets', [])
    table_data = section.get('table', None)
    image_path = section.get('image_path', '')
    page_break = section.get('page_break', False)

    if page_break:
        doc.add_page_break()

    if heading:
        doc.add_heading(heading, level=min(level, 9))

    if content:
        para = doc.add_paragraph(content)
        para.style.font.name = font_name
        para.style.font.size = Pt(font_size)

    for bullet in bullets:
        if isinstance(bullet, dict):
            text = bullet.get('text', str(bullet))
            level_b = bullet.get('level', 0)
        else:
            text = str(bullet)
            level_b = 0
        para = doc.add_paragraph(text, style='List Bullet')

    if table_data:
        headers = table_data.get('headers', [])
        rows = table_data.get('rows', [])
        if headers:
            table = doc.add_table(rows=1, cols=len(headers), style='Table Grid')
            hdr_cells = table.rows[0].cells
            for i, h in enumerate(headers):
                hdr_cells[i].text = str(h)
                for para in hdr_cells[i].paragraphs:
                    for run in para.runs:
                        run.bold = True
            for row_data in rows:
                row_cells = table.add_row().cells
                for i, val in enumerate(row_data):
                    if i < len(row_cells):
                        row_cells[i].text = str(val)

    if image_path:
        try:
            import os
            if os.path.exists(image_path):
                doc.add_picture(image_path, width=Inches(5))
        except Exception:
            pass

doc.save(output_path)
print('OK')
"#,
        sections_json = sections_json.replace('\\', "\\\\").replace('\'', "\\'"),
        style_json = style_json.replace('\\', "\\\\").replace('\'', "\\'"),
        title = quote_python_str(title),
        output_path = quote_python_str(&output_path),
    );

    run_python_script(&script, &output_path, "docx", sections.len(), ctx).await
}

/// Create an XLSX spreadsheet.
async fn action_create_xlsx(ctx: &ToolContext, params: &Value) -> Result<Value> {
    let sheets = params.get("sheets").and_then(|v| v.as_array())
        .ok_or_else(|| Error::Tool("'sheets' is required".into()))?;
    let style = params.get("style").cloned().unwrap_or(json!({}));

    let output_path = resolve_output_path(params, ctx, "xlsx", "spreadsheet");

    let sheets_json = serde_json::to_string(sheets)
        .map_err(|e| Error::Tool(format!("Failed to serialize sheets: {}", e)))?;
    let style_json = serde_json::to_string(&style)
        .map_err(|e| Error::Tool(format!("Failed to serialize style: {}", e)))?;

    let script = format!(
        r#"
import json
from openpyxl import Workbook
from openpyxl.styles import Font, Alignment, PatternFill, Border, Side
from openpyxl.utils import get_column_letter

sheets_data = json.loads('''{sheets_json}''')
style = json.loads('''{style_json}''')
output_path = {output_path}

wb = Workbook()

font_name = style.get('font', 'Calibri')
font_size = style.get('font_size', 11)

# Remove default sheet if we have data
if sheets_data:
    wb.remove(wb.active)

for sheet_data in sheets_data:
    name = sheet_data.get('name', 'Sheet')
    headers = sheet_data.get('headers', [])
    rows = sheet_data.get('rows', [])
    column_widths = sheet_data.get('column_widths', [])
    bold_header = sheet_data.get('bold_header', True)

    ws = wb.create_sheet(title=name)

    header_font = Font(name=font_name, size=font_size, bold=bold_header)
    header_fill = PatternFill(start_color='4472C4', end_color='4472C4', fill_type='solid')
    header_font_white = Font(name=font_name, size=font_size, bold=True, color='FFFFFF')
    cell_font = Font(name=font_name, size=font_size)
    thin_border = Border(
        left=Side(style='thin'),
        right=Side(style='thin'),
        top=Side(style='thin'),
        bottom=Side(style='thin')
    )

    # Write headers
    if headers:
        for col_idx, header in enumerate(headers, 1):
            cell = ws.cell(row=1, column=col_idx, value=str(header))
            cell.font = header_font_white
            cell.fill = header_fill
            cell.alignment = Alignment(horizontal='center')
            cell.border = thin_border

    # Write data rows
    start_row = 2 if headers else 1
    for row_idx, row_data in enumerate(rows, start_row):
        for col_idx, value in enumerate(row_data, 1):
            cell = ws.cell(row=row_idx, column=col_idx, value=value)
            cell.font = cell_font
            cell.border = thin_border

    # Set column widths
    if column_widths:
        for col_idx, width in enumerate(column_widths, 1):
            ws.column_dimensions[get_column_letter(col_idx)].width = width
    else:
        # Auto-width based on content
        for col_idx in range(1, len(headers) + 1 if headers else (max(len(r) for r in rows) + 1 if rows else 1)):
            max_len = 0
            col_letter = get_column_letter(col_idx)
            for row in ws.iter_rows(min_col=col_idx, max_col=col_idx):
                for cell in row:
                    if cell.value:
                        max_len = max(max_len, len(str(cell.value)))
            ws.column_dimensions[col_letter].width = min(max(max_len + 2, 8), 50)

    # Freeze header row
    if headers:
        ws.freeze_panes = 'A2'

    # Auto-filter
    if headers and rows:
        ws.auto_filter.ref = f'A1:{{get_column_letter(len(headers))}}{{len(rows) + 1}}'

wb.save(output_path)
print('OK')
"#,
        sheets_json = sheets_json.replace('\\', "\\\\").replace('\'', "\\'"),
        style_json = style_json.replace('\\', "\\\\").replace('\'', "\\'"),
        output_path = quote_python_str(&output_path),
    );

    run_python_script(&script, &output_path, "xlsx", sheets.len(), ctx).await
}

/// Resolve output path from params or generate default.
fn resolve_output_path(params: &Value, ctx: &ToolContext, ext: &str, prefix: &str) -> String {
    if let Some(op) = params.get("output_path").and_then(|v| v.as_str()) {
        if !op.is_empty() {
            return expand_path(op, ctx);
        }
    }
    let dir = ctx.workspace.join("documents");
    let _ = std::fs::create_dir_all(&dir);
    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
    dir.join(format!("{}_{}.{}", prefix, timestamp, ext))
        .to_string_lossy().to_string()
}

fn expand_path(path: &str, ctx: &ToolContext) -> String {
    if path.starts_with('~') {
        if let Some(home) = dirs::home_dir() {
            return path.replacen('~', &home.to_string_lossy(), 1);
        }
    }
    if std::path::Path::new(path).is_absolute() {
        return path.to_string();
    }
    ctx.workspace.join(path).to_string_lossy().to_string()
}

/// Run a Python script and return the result.
async fn run_python_script(script: &str, output_path: &str, format: &str, item_count: usize, ctx: &ToolContext) -> Result<Value> {
    let python_bin = if which::which("python3").is_ok() { "python3" } else { "python" };
    if which::which(python_bin).is_err() {
        return Err(Error::Tool("Python not found. Install Python 3 to generate Office documents.".into()));
    }

    // Write script to temp file
    let script_dir = ctx.workspace.join("tmp");
    let _ = std::fs::create_dir_all(&script_dir);
    let script_path = script_dir.join("_office_gen.py");
    std::fs::write(&script_path, script)
        .map_err(|e| Error::Tool(format!("Failed to write script: {}", e)))?;

    if let Some(parent) = std::path::Path::new(output_path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    info!(format = %format, output = %output_path, items = item_count, "📄 Generating Office document");

    let output = tokio::process::Command::new(python_bin)
        .arg(&script_path)
        .output()
        .await
        .map_err(|e| Error::Tool(format!("Python execution failed: {}", e)))?;

    // Clean up
    let _ = std::fs::remove_file(&script_path);

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Tool(format!(
            "Office document generation failed: {}",
            truncate_str(&stderr, 500)
        )));
    }

    if !std::path::Path::new(output_path).exists() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Tool(format!(
            "File not created at {}. stdout: {} stderr: {}",
            output_path,
            truncate_str(&stdout, 200),
            truncate_str(&stderr, 200)
        )));
    }

    let file_size = std::fs::metadata(output_path).map(|m| m.len()).unwrap_or(0);

    info!(path = %output_path, size = file_size, "📄 Office document generated");

    Ok(json!({
        "success": true,
        "format": format,
        "output_path": output_path,
        "file_size_bytes": file_size,
        "item_count": item_count,
    }))
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
        let tool = OfficeWriteTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "office_write");
    }

    #[test]
    fn test_validate_pptx() {
        let tool = OfficeWriteTool;
        assert!(tool.validate(&json!({
            "action": "create_pptx",
            "slides": [{"layout": "title", "title": "Hello"}]
        })).is_ok());
        assert!(tool.validate(&json!({"action": "create_pptx", "slides": []})).is_err());
        assert!(tool.validate(&json!({"action": "create_pptx"})).is_err());
    }

    #[test]
    fn test_validate_docx() {
        let tool = OfficeWriteTool;
        assert!(tool.validate(&json!({
            "action": "create_docx",
            "sections": [{"heading": "Intro", "content": "Hello"}]
        })).is_ok());
        assert!(tool.validate(&json!({"action": "create_docx", "sections": []})).is_err());
    }

    #[test]
    fn test_validate_xlsx() {
        let tool = OfficeWriteTool;
        assert!(tool.validate(&json!({
            "action": "create_xlsx",
            "sheets": [{"name": "Data", "headers": ["A", "B"], "rows": [[1, 2]]}]
        })).is_ok());
        assert!(tool.validate(&json!({"action": "create_xlsx", "sheets": []})).is_err());
    }

    #[test]
    fn test_validate_info() {
        let tool = OfficeWriteTool;
        assert!(tool.validate(&json!({"action": "info"})).is_ok());
        assert!(tool.validate(&json!({"action": "invalid"})).is_err());
    }

    #[test]
    fn test_quote_python_str() {
        assert_eq!(quote_python_str("hello"), "'hello'");
        assert_eq!(quote_python_str("it's a \"test\""), "'it\\'s a \"test\"'");
    }
}
