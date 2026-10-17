use blockcell_core::{Error, Result};
use calamine::{open_workbook_auto, Data, Reader};
use std::io::{BufReader, Read};
use std::path::Path;

/// Upper bound for a single decompressed OOXML part. Office files are ZIP archives, and
/// a high-ratio document would otherwise expand into gigabytes ("zip bomb").
const MAX_OOXML_ENTRY_BYTES: u64 = 64 * 1024 * 1024;
/// Upper bound on the number of slides we walk in one presentation.
const MAX_PPTX_SLIDES: usize = 500;
/// Upper bound for the on-disk size of a spreadsheet handed to calamine (which loads
/// the whole workbook into memory).
const MAX_EXCEL_FILE_BYTES: u64 = 128 * 1024 * 1024;

/// Read a ZIP entry into a String, refusing anything that decompresses past
/// [`MAX_OOXML_ENTRY_BYTES`].
fn read_zip_entry(entry: &mut impl Read, name: &str) -> Result<String> {
    let mut buffer = Vec::new();
    entry
        .take(MAX_OOXML_ENTRY_BYTES + 1)
        .read_to_end(&mut buffer)
        .map_err(|e| Error::Tool(format!("Failed to read {}: {}", name, e)))?;
    if buffer.len() as u64 > MAX_OOXML_ENTRY_BYTES {
        return Err(Error::Tool(format!(
            "{} exceeds the {} byte decompression limit",
            name, MAX_OOXML_ENTRY_BYTES
        )));
    }
    String::from_utf8(buffer)
        .map_err(|e| Error::Tool(format!("{} is not valid UTF-8: {}", name, e)))
}

/// Read an Excel file (.xlsx, .xls) and return its content as text.
pub fn read_excel(path: &Path) -> Result<String> {
    let size = std::fs::metadata(path)
        .map_err(|e| Error::Tool(format!("Failed to stat Excel file: {}", e)))?
        .len();
    if size > MAX_EXCEL_FILE_BYTES {
        return Err(Error::Tool(format!(
            "Excel file is {} bytes, above the {} byte limit",
            size, MAX_EXCEL_FILE_BYTES
        )));
    }

    let mut workbook = open_workbook_auto(path)
        .map_err(|e| Error::Tool(format!("Failed to open Excel file: {}", e)))?;

    let mut output = String::new();
    let sheet_names = workbook.sheet_names().to_vec();

    for (idx, name) in sheet_names.iter().enumerate() {
        if idx > 0 {
            output.push_str("\n\n");
        }
        output.push_str(&format!("## Sheet: {}\n\n", name));

        if let Ok(range) = workbook.worksheet_range(name) {
            let mut rows_output = Vec::new();
            for row in range.rows() {
                let cells: Vec<String> = row
                    .iter()
                    .map(|cell| match cell {
                        Data::Empty => String::new(),
                        Data::String(s) => s.clone(),
                        Data::Float(f) => {
                            if *f == (*f as i64) as f64 {
                                format!("{}", *f as i64)
                            } else {
                                format!("{}", f)
                            }
                        }
                        Data::Int(i) => format!("{}", i),
                        Data::Bool(b) => format!("{}", b),
                        Data::DateTime(dt) => format!("{}", dt),
                        Data::DateTimeIso(s) => s.clone(),
                        Data::DurationIso(s) => s.clone(),
                        Data::Error(e) => format!("#ERR:{:?}", e),
                    })
                    .collect();
                rows_output.push(cells.join("\t"));
            }

            // Format as markdown table if we have data
            if !rows_output.is_empty() {
                // Use first row as header
                let header = &rows_output[0];
                let col_count = header.split('\t').count();
                output.push_str("| ");
                output.push_str(&header.replace('\t', " | "));
                output.push_str(" |\n");

                // Separator
                output.push('|');
                for _ in 0..col_count {
                    output.push_str(" --- |");
                }
                output.push('\n');

                // Data rows
                for row in &rows_output[1..] {
                    output.push_str("| ");
                    output.push_str(&row.replace('\t', " | "));
                    output.push_str(" |\n");
                }
            }
        }
    }

    Ok(output)
}

/// Read a .docx file and return its text content.
pub fn read_docx(path: &Path) -> Result<String> {
    let file = std::fs::File::open(path)
        .map_err(|e| Error::Tool(format!("Failed to open docx file: {}", e)))?;
    let reader = BufReader::new(file);
    let mut archive = zip::ZipArchive::new(reader)
        .map_err(|e| Error::Tool(format!("Failed to read docx as ZIP: {}", e)))?;

    // The main document content is in word/document.xml
    let xml_content = {
        let mut doc_file = archive
            .by_name("word/document.xml")
            .map_err(|e| Error::Tool(format!("Failed to find document.xml in docx: {}", e)))?;
        read_zip_entry(&mut doc_file, "word/document.xml")?
    };

    extract_text_from_xml(&xml_content, "w:t")
}

/// Read a .pptx file and return its text content.
pub fn read_pptx(path: &Path) -> Result<String> {
    let file = std::fs::File::open(path)
        .map_err(|e| Error::Tool(format!("Failed to open pptx file: {}", e)))?;
    let reader = BufReader::new(file);
    let mut archive = zip::ZipArchive::new(reader)
        .map_err(|e| Error::Tool(format!("Failed to read pptx as ZIP: {}", e)))?;

    // Collect slide file names (ppt/slides/slide1.xml, slide2.xml, ...)
    let mut slide_names: Vec<String> = Vec::new();
    for i in 0..archive.len() {
        if let Ok(file) = archive.by_index(i) {
            let name = file.name().to_string();
            if name.starts_with("ppt/slides/slide") && name.ends_with(".xml") {
                slide_names.push(name);
            }
        }
    }
    slide_names.sort();
    if slide_names.len() > MAX_PPTX_SLIDES {
        return Err(Error::Tool(format!(
            "presentation has {} slides, above the {} slide limit",
            slide_names.len(),
            MAX_PPTX_SLIDES
        )));
    }

    let mut output = String::new();
    for (idx, slide_name) in slide_names.iter().enumerate() {
        if idx > 0 {
            output.push_str("\n\n");
        }
        output.push_str(&format!("## Slide {}\n\n", idx + 1));

        let xml_content = {
            let mut slide_file = archive
                .by_name(slide_name)
                .map_err(|e| Error::Tool(format!("Failed to read {}: {}", slide_name, e)))?;
            read_zip_entry(&mut slide_file, slide_name)?
        };

        match extract_text_from_xml(&xml_content, "a:t") {
            Ok(text) => output.push_str(&text),
            Err(_) => output.push_str("(unable to extract text)"),
        }
    }

    Ok(output)
}

/// Extract text content from XML by collecting all text nodes with the given tag name.
/// For docx, the tag is "w:t"; for pptx, the tag is "a:t".
fn extract_text_from_xml(xml: &str, target_tag: &str) -> Result<String> {
    use quick_xml::events::Event;
    use quick_xml::reader::Reader;

    let mut reader = Reader::from_str(xml);
    let mut output = String::new();
    let mut inside_target = false;
    let mut current_paragraph = String::new();
    let mut buf = Vec::new();

    // Determine the paragraph tag based on the text tag
    let para_tag = if target_tag == "w:t" { "w:p" } else { "a:p" };

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                let local_name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if local_name == target_tag {
                    inside_target = true;
                }
            }
            Ok(Event::End(ref e)) => {
                let local_name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if local_name == target_tag {
                    inside_target = false;
                } else if local_name == para_tag {
                    // End of paragraph — flush
                    let trimmed = current_paragraph.trim().to_string();
                    if !trimmed.is_empty() {
                        if !output.is_empty() {
                            output.push('\n');
                        }
                        output.push_str(&trimmed);
                    }
                    current_paragraph.clear();
                }
            }
            Ok(Event::Text(ref e)) if inside_target => {
                if let Ok(text) = e.unescape() {
                    current_paragraph.push_str(&text);
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                return Err(Error::Tool(format!("XML parse error: {}", e)));
            }
            _ => {}
        }
        buf.clear();
    }

    // Flush any remaining text
    let trimmed = current_paragraph.trim().to_string();
    if !trimmed.is_empty() {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&trimmed);
    }

    Ok(output)
}

/// Check if a file extension indicates an office document we can read.
pub fn is_office_file(path: &Path) -> bool {
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => matches!(
            ext.to_lowercase().as_str(),
            "xlsx" | "xls" | "docx" | "pptx"
        ),
        None => false,
    }
}

/// Read an office file and return its text content.
pub fn read_office_file(path: &Path) -> Result<String> {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
    {
        Some(ext) if ext == "xlsx" || ext == "xls" => read_excel(path),
        Some(ext) if ext == "docx" => read_docx(path),
        Some(ext) if ext == "pptx" => read_pptx(path),
        _ => Err(Error::Tool(format!(
            "Unsupported office file format: {}",
            path.display()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_is_office_file() {
        assert!(is_office_file(Path::new("test.xlsx")));
        assert!(is_office_file(Path::new("test.xls")));
        assert!(is_office_file(Path::new("test.docx")));
        assert!(is_office_file(Path::new("test.pptx")));
        assert!(is_office_file(Path::new("TEST.XLSX")));
        assert!(!is_office_file(Path::new("test.txt")));
        assert!(!is_office_file(Path::new("test.pdf")));
        assert!(!is_office_file(Path::new("test")));
    }

    #[test]
    fn test_read_office_file_unsupported() {
        let result = read_office_file(Path::new("/tmp/test.txt"));
        assert!(result.is_err());
    }

    #[test]
    fn test_read_office_file_missing() {
        let result = read_office_file(Path::new("/tmp/nonexistent.xlsx"));
        assert!(result.is_err());
    }
}
