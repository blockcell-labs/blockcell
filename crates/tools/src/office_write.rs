use async_trait::async_trait;
use blockcell_core::{Error, Result};
use serde_json::{json, Value};
use tracing::info;

use crate::{Tool, ToolContext, ToolSchema};

/// Tool for generating Office documents (PPTX, DOCX, XLSX).
///
/// DOCX is generated natively via docx-rs; PPTX / XLSX use Python libraries
/// (python-pptx, openpyxl) to create properly formatted Office files from structured data.
pub struct OfficeWriteTool;

#[async_trait]
impl Tool for OfficeWriteTool {
    fn workspace_effect(&self, params: &Value) -> crate::evidence::WorkspaceEffect {
        if params["action"] != "info" {
            crate::evidence::WorkspaceEffect::Changed
        } else {
            crate::evidence::WorkspaceEffect::None
        }
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "office_write".to_string(),
            description: "Generate Office documents. You MUST provide `action`. action='info': no extra params. action='create_pptx': requires `slides`, optional `title`, `output_path`, and `style`. action='create_docx': requires `sections`, optional `title`, `output_path`, and `style`. action='create_xlsx': requires `sheets`, optional `title`, `output_path`, and `style`.".to_string(),
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
                        "items": { "type": "object" },
                        "description": "For PPTX: array of slides. Each slide: {layout, title, content, bullets, image_path, notes, table}. layout: 'title'|'content'|'two_content'|'blank'|'section'"
                    },
                    "sections": {
                        "type": "array",
                        "items": { "type": "object" },
                        "description": "For DOCX: array of sections. Each: {heading, level, content, bullets, table, image_path, page_break}"
                    },
                    "sheets": {
                        "type": "array",
                        "items": { "type": "object" },
                        "description": "For XLSX: array of sheets. Each: {name, headers, rows, column_widths, bold_header}"
                    },
                    "style": {
                        "oneOf": [
                            {
                                "type": "object",
                                "description": "Style options: {font, font_size, theme_color, author}"
                            },
                            {
                                "type": "string",
                                "description": "Preset style name, e.g. 'professional' or 'modern'"
                            }
                        ],
                        "description": "Style options as an object or a preset string"
                    }
                },
                "required": ["action"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("");
        if !["create_pptx", "create_docx", "create_xlsx", "info"].contains(&action) {
            return Err(Error::Tool(
                "action must be 'create_pptx', 'create_docx', 'create_xlsx', or 'info'".into(),
            ));
        }
        match action {
            "create_pptx"
                if params
                    .get("slides")
                    .and_then(|v| v.as_array())
                    .map(|a| a.is_empty())
                    .unwrap_or(true) =>
            {
                return Err(Error::Tool(
                    "'slides' array is required for create_pptx".into(),
                ));
            }
            "create_docx"
                if params
                    .get("sections")
                    .and_then(|v| v.as_array())
                    .map(|a| a.is_empty())
                    .unwrap_or(true) =>
            {
                return Err(Error::Tool(
                    "'sections' array is required for create_docx".into(),
                ));
            }
            "create_xlsx"
                if params
                    .get("sheets")
                    .and_then(|v| v.as_array())
                    .map(|a| a.is_empty())
                    .unwrap_or(true) =>
            {
                return Err(Error::Tool(
                    "'sheets' array is required for create_xlsx".into(),
                ));
            }
            _ => {}
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("info");

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
    let python_bin = if which::which("python3").is_ok() {
        "python3"
    } else {
        "python"
    };

    let mut libs = json!({});

    if has_python {
        for (pkg, import) in &[("python-pptx", "pptx"), ("openpyxl", "openpyxl")] {
            let check = tokio::process::Command::new(python_bin)
                .args([
                    "-c",
                    &format!("import {}; print({}.__version__)", import, import),
                ])
                .output()
                .await;
            let available = check.map(|o| o.status.success()).unwrap_or(false);
            libs[*pkg] = json!(available);
        }
    }

    let all_installed = libs
        .get("python-pptx")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        && libs
            .get("openpyxl")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

    let install_hint = if !all_installed {
        "Install: pip install python-pptx openpyxl"
    } else {
        ""
    };

    Ok(json!({
        "has_python": has_python,
        "libraries": libs,
        "all_installed": all_installed,
        "supported_formats": ["pptx", "docx", "xlsx"],
        "native_formats": ["docx"],
        "install_hint": install_hint,
    }))
}

/// Create a PPTX presentation.
async fn action_create_pptx(ctx: &ToolContext, params: &Value) -> Result<Value> {
    let slides = params
        .get("slides")
        .and_then(|v| v.as_array())
        .ok_or_else(|| Error::Tool("'slides' is required".into()))?;
    let title = params
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("Presentation");
    let style = normalize_style(params.get("style"));

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

/// Create a DOCX document natively via docx-rs (no Python required).
async fn action_create_docx(ctx: &ToolContext, params: &Value) -> Result<Value> {
    let sections = params
        .get("sections")
        .and_then(|v| v.as_array())
        .ok_or_else(|| Error::Tool("'sections' is required".into()))?
        .clone();
    let title = params
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let style = normalize_style(params.get("style"));
    let output_path = resolve_output_path(params, ctx, "docx", "document");
    let workspace = ctx.workspace.clone();
    let item_count = sections.len();

    info!(format = "docx", output = %output_path, items = item_count, "📄 Generating Office document");

    let path = output_path.clone();
    let file_size = tokio::task::spawn_blocking(move || -> Result<u64> {
        let bytes = build_docx(&title, &sections, &style, &workspace)?;
        if let Some(parent) = std::path::Path::new(&path).parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, &bytes)?;
        Ok(bytes.len() as u64)
    })
    .await
    .map_err(|e| Error::Tool(format!("DOCX generation task failed: {}", e)))??;

    info!(path = %output_path, size = file_size, "📄 Office document generated");

    Ok(json!({
        "success": true,
        "format": "docx",
        "output_path": output_path,
        "file_size_bytes": file_size,
        "item_count": item_count,
    }))
}

const BULLET_NUMBERING_ID: usize = 1;
/// 表格总宽（twip），约等于 A4 减去默认页边距。
const TABLE_WIDTH_DXA: usize = 9000;
/// 图片宽度 5 英寸（EMU）。
const IMAGE_WIDTH_EMU: u32 = 5 * 914_400;
const HEADING_COLOR: &str = "2F5496";

fn build_docx(
    title: &str,
    sections: &[Value],
    style: &Value,
    workspace: &std::path::Path,
) -> Result<Vec<u8>> {
    use docx_rs::{BreakType, Docx, IndentLevel, Numbering, NumberingId, Paragraph, Run, RunFonts};

    let font = style
        .get("font")
        .and_then(Value::as_str)
        .unwrap_or("Calibri");
    let font_size = style
        .get("font_size")
        .and_then(Value::as_f64)
        .unwrap_or(11.0);
    let author = style.get("author").and_then(Value::as_str).unwrap_or("");
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    let mut docx = Docx::new()
        .default_fonts(
            RunFonts::new()
                .ascii(font)
                .hi_ansi(font)
                .east_asia(font)
                .cs(font),
        )
        .default_size((font_size * 2.0).round() as usize)
        .created_at(&now)
        .updated_at(&now)
        .add_style(paragraph_style("Title", "Title", 56, None))
        .add_abstract_numbering(bullet_numbering())
        .add_numbering(Numbering::new(BULLET_NUMBERING_ID, BULLET_NUMBERING_ID));
    for level in 1..=9usize {
        let size = match level {
            1 => 32,
            2 => 26,
            3 => 24,
            _ => 22,
        };
        docx = docx.add_style(paragraph_style(
            &format!("Heading{level}"),
            &format!("heading {level}"),
            size,
            Some(level - 1),
        ));
    }

    if !title.is_empty() {
        docx = docx.add_paragraph(text_paragraph(title).style("Title"));
    }

    for section in sections {
        if section["page_break"].as_bool().unwrap_or(false) {
            docx =
                docx.add_paragraph(Paragraph::new().add_run(Run::new().add_break(BreakType::Page)));
        }
        if let Some(heading) = non_empty_str(&section["heading"]) {
            let level = section["level"].as_u64().unwrap_or(1).clamp(1, 9);
            docx = docx.add_paragraph(text_paragraph(heading).style(&format!("Heading{level}")));
        }
        if let Some(content) = non_empty_str(&section["content"]) {
            docx = docx.add_paragraph(text_paragraph(content));
        }
        for bullet in section["bullets"].as_array().into_iter().flatten() {
            let (text, level) = match bullet {
                Value::Object(map) => (
                    map.get("text")
                        .map(value_text)
                        .unwrap_or_else(|| bullet.to_string()),
                    bullet["level"].as_u64().unwrap_or(0).min(8) as usize,
                ),
                _ => (value_text(bullet), 0),
            };
            docx = docx.add_paragraph(text_paragraph(&text).numbering(
                NumberingId::new(BULLET_NUMBERING_ID),
                IndentLevel::new(level),
            ));
        }
        if let Some(table) = build_docx_table(&section["table"]) {
            docx = docx.add_table(table);
        }
        if let Some(image_path) = non_empty_str(&section["image_path"]) {
            if let Some(pic) = load_docx_picture(&expand_path_string(image_path, workspace)) {
                docx = docx.add_paragraph(Paragraph::new().add_run(Run::new().add_image(pic)));
            }
        }
    }

    let mut xml = docx.build();
    if !author.is_empty() {
        // docx-rs 不提供设置作者的公开 API，直接改写 core.xml 中的默认值。
        let core = String::from_utf8_lossy(&xml.doc_props.core).replace(
            "<dc:creator>unknown</dc:creator>",
            &format!(
                "<dc:creator>{}</dc:creator>",
                quick_xml::escape::escape(author)
            ),
        );
        xml.doc_props.core = core.into_bytes();
    }
    let mut out = std::io::Cursor::new(Vec::new());
    xml.pack(&mut out)
        .map_err(|e| Error::Tool(format!("Failed to pack docx: {}", e)))?;
    Ok(out.into_inner())
}

fn paragraph_style(id: &str, name: &str, size: usize, outline: Option<usize>) -> docx_rs::Style {
    let style = docx_rs::Style::new(id, docx_rs::StyleType::Paragraph)
        .name(name)
        .size(size);
    match outline {
        Some(level) => style.bold().color(HEADING_COLOR).outline_lvl(level),
        None => style,
    }
}

fn bullet_numbering() -> docx_rs::AbstractNumbering {
    use docx_rs::{Level, LevelJc, LevelText, NumberFormat, SpecialIndentType, Start};
    (0..9).fold(
        docx_rs::AbstractNumbering::new(BULLET_NUMBERING_ID),
        |numbering, level| {
            let symbol = ["•", "◦", "▪"][level % 3];
            numbering.add_level(
                Level::new(
                    level,
                    Start::new(1),
                    NumberFormat::new("bullet"),
                    LevelText::new(symbol),
                    LevelJc::new("left"),
                )
                .indent(
                    Some(720 * (level as i32 + 1)),
                    Some(SpecialIndentType::Hanging(360)),
                    None,
                    None,
                ),
            )
        },
    )
}

/// 文本中的换行转为段内换行。
fn text_paragraph(text: &str) -> docx_rs::Paragraph {
    let mut run = docx_rs::Run::new();
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            run = run.add_break(docx_rs::BreakType::TextWrapping);
        }
        run = run.add_text(line);
    }
    docx_rs::Paragraph::new().add_run(run)
}

fn build_docx_table(data: &Value) -> Option<docx_rs::Table> {
    use docx_rs::{Paragraph, Run, Table, TableCell, TableRow, WidthType};
    let headers = data
        .get("headers")?
        .as_array()
        .filter(|headers| !headers.is_empty())?;
    let cell = |text: String, bold: bool| {
        let run = Run::new().add_text(text);
        TableCell::new().add_paragraph(Paragraph::new().add_run(if bold {
            run.bold()
        } else {
            run
        }))
    };
    let mut rows = vec![TableRow::new(
        headers.iter().map(|h| cell(value_text(h), true)).collect(),
    )];
    for values in data
        .get("rows")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_array)
    {
        rows.push(TableRow::new(
            (0..headers.len())
                .map(|i| cell(values.get(i).map(value_text).unwrap_or_default(), false))
                .collect(),
        ));
    }
    Some(
        Table::new(rows)
            .set_grid(vec![TABLE_WIDTH_DXA / headers.len(); headers.len()])
            .width(TABLE_WIDTH_DXA, WidthType::Dxa),
    )
}

/// 读取并校验图片；缺失或无法解码时跳过（docx-rs 的 `Pic::new` 遇到坏图会 panic）。
fn load_docx_picture(path: &str) -> Option<docx_rs::Pic> {
    let bytes = std::fs::read(path)
        .map_err(|e| tracing::warn!(path, error = %e, "Skipping docx image: unreadable"))
        .ok()?;
    let image = image::load_from_memory(&bytes)
        .map_err(|e| tracing::warn!(path, error = %e, "Skipping docx image: undecodable"))
        .ok()?;
    if image.width() == 0 {
        return None;
    }
    let height =
        (u64::from(IMAGE_WIDTH_EMU) * u64::from(image.height()) / u64::from(image.width())) as u32;
    Some(docx_rs::Pic::new(&bytes).size(IMAGE_WIDTH_EMU, height))
}

fn non_empty_str(value: &Value) -> Option<&str> {
    value.as_str().filter(|s| !s.is_empty())
}

/// 字符串原样输出，其他 JSON 值按其 JSON 文本输出。
fn value_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Create an XLSX spreadsheet.
async fn action_create_xlsx(ctx: &ToolContext, params: &Value) -> Result<Value> {
    let sheets = params
        .get("sheets")
        .and_then(|v| v.as_array())
        .ok_or_else(|| Error::Tool("'sheets' is required".into()))?;
    let style = normalize_style(params.get("style"));

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
            return crate::expand_path(op, &ctx.workspace)
                .to_string_lossy()
                .into_owned();
        }
    }
    let dir = ctx.workspace.join("documents");
    let _ = std::fs::create_dir_all(&dir);
    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
    dir.join(format!("{}_{}.{}", prefix, timestamp, ext))
        .to_string_lossy()
        .to_string()
}

fn expand_path_string(path: &str, workspace: &std::path::Path) -> String {
    crate::expand_path(path, workspace)
        .to_string_lossy()
        .into_owned()
}

/// Run a Python script and return the result.
async fn run_python_script(
    script: &str,
    output_path: &str,
    format: &str,
    item_count: usize,
    ctx: &ToolContext,
) -> Result<Value> {
    let python_bin = if which::which("python3").is_ok() {
        "python3"
    } else {
        "python"
    };
    if which::which(python_bin).is_err() {
        return Err(Error::Tool(
            "Python not found. Install Python 3 to generate Office documents.".into(),
        ));
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
    format!("'{}'", crate::escape_python_string(s))
}

fn normalize_style(style: Option<&Value>) -> Value {
    match style {
        Some(Value::Object(map)) => Value::Object(map.clone()),
        Some(Value::String(name)) => style_preset(name),
        _ => json!({}),
    }
}

fn style_preset(name: &str) -> Value {
    let preset = name.trim().to_ascii_lowercase();
    match preset.as_str() {
        "professional" => json!({
            "font": "Calibri",
            "theme_color": "2F5597"
        }),
        "modern" => json!({
            "font": "Aptos",
            "theme_color": "4472C4"
        }),
        "minimal" => json!({
            "font": "Calibri"
        }),
        _ => json!({}),
    }
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
    fn p3_tilde_prefix_is_a_relative_filename() {
        let workspace = std::path::Path::new("/tmp/workspace");
        assert_eq!(
            expand_path_string("~report.docx", workspace),
            "/tmp/workspace/~report.docx"
        );
    }

    #[test]
    fn test_schema() {
        let tool = OfficeWriteTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "office_write");
    }

    #[test]
    fn test_validate_pptx() {
        let tool = OfficeWriteTool;
        assert!(tool
            .validate(&json!({
                "action": "create_pptx",
                "slides": [{"layout": "title", "title": "Hello"}]
            }))
            .is_ok());
        assert!(tool
            .validate(&json!({"action": "create_pptx", "slides": []}))
            .is_err());
        assert!(tool.validate(&json!({"action": "create_pptx"})).is_err());
    }

    #[test]
    fn test_validate_docx() {
        let tool = OfficeWriteTool;
        assert!(tool
            .validate(&json!({
                "action": "create_docx",
                "sections": [{"heading": "Intro", "content": "Hello"}]
            }))
            .is_ok());
        assert!(tool
            .validate(&json!({"action": "create_docx", "sections": []}))
            .is_err());
    }

    #[test]
    fn test_validate_xlsx() {
        let tool = OfficeWriteTool;
        assert!(tool
            .validate(&json!({
                "action": "create_xlsx",
                "sheets": [{"name": "Data", "headers": ["A", "B"], "rows": [[1, 2]]}]
            }))
            .is_ok());
        assert!(tool
            .validate(&json!({"action": "create_xlsx", "sheets": []}))
            .is_err());
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
        // `\"` inside a single-quoted Python literal still reads back as `"`.
        assert_eq!(
            quote_python_str("it's a \"test\""),
            "'it\\'s a \\\"test\\\"'"
        );
        assert_eq!(quote_python_str("a\nb"), "'a\\nb'");
    }

    #[test]
    fn test_normalize_style_object_passthrough() {
        let style = normalize_style(Some(&json!({
            "font": "Arial",
            "font_size": 20
        })));
        assert_eq!(style["font"], "Arial");
        assert_eq!(style["font_size"], 20);
    }

    #[test]
    fn test_normalize_style_string_preset() {
        let style = normalize_style(Some(&json!("professional")));
        assert_eq!(style["font"], "Calibri");
        assert_eq!(style["theme_color"], "2F5597");
    }

    fn docx_part(bytes: &[u8], name: &str) -> String {
        use std::io::Read;
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut part = String::new();
        archive
            .by_name(name)
            .unwrap()
            .read_to_string(&mut part)
            .unwrap();
        part
    }

    #[test]
    fn build_docx_renders_sections_natively() {
        let dir = tempfile::tempdir().unwrap();
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(4, 2)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        std::fs::write(dir.path().join("chart.png"), png.into_inner()).unwrap();
        std::fs::write(dir.path().join("broken.png"), b"not an image").unwrap();

        let sections = json!([
            {"heading": "Intro", "level": 2, "content": "line one\nline two", "page_break": true},
            {"bullets": ["first", {"text": "nested", "level": 1}, 42]},
            {"table": {"headers": ["Name", "Score"], "rows": [["a", 1], ["b"]]}},
            {"image_path": "chart.png"},
            {"image_path": "broken.png"},
            {"image_path": "missing.png"}
        ]);
        let bytes = build_docx(
            "Report <Q1>",
            sections.as_array().unwrap(),
            &json!({"font": "Arial", "font_size": 12, "author": "Ann & Bob"}),
            dir.path(),
        )
        .unwrap();

        let document = docx_part(&bytes, "word/document.xml");
        assert!(
            document.contains(r#"<w:pStyle w:val="Title" />"#),
            "{document}"
        );
        assert!(document.contains(r#"<w:pStyle w:val="Heading2" />"#));
        assert!(document.contains(r#"<w:br w:type="page" />"#));
        assert!(document.contains(r#"<w:br w:type="textWrapping" />"#));
        assert!(document.contains(r#"<w:ilvl w:val="1" />"#));
        assert!(document.contains(">42<"));
        assert!(document.contains("<w:b />"));
        assert_eq!(document.matches("<w:tr>").count(), 3);
        assert_eq!(document.matches("<w:drawing>").count(), 1);
        assert!(docx_part(&bytes, "word/styles.xml").contains(r#"w:ascii="Arial""#));
        assert!(docx_part(&bytes, "word/styles.xml").contains(r#"<w:sz w:val="24" />"#));
        assert!(docx_part(&bytes, "word/numbering.xml").contains("•"));
        assert!(docx_part(&bytes, "docProps/core.xml")
            .contains("<dc:creator>Ann &amp; Bob</dc:creator>"));

        let path = dir.path().join("out.docx");
        std::fs::write(&path, &bytes).unwrap();
        let text = crate::office::read_docx(&path).unwrap();
        for expected in ["Report <Q1>", "Intro", "line one", "nested", "Score"] {
            assert!(text.contains(expected), "{expected} missing in {text}");
        }
    }

    #[test]
    fn test_normalize_style_unknown_string() {
        let style = normalize_style(Some(&json!("unknown")));
        assert_eq!(style, json!({}));
    }
}
