use async_trait::async_trait;
use blockcell_core::{Error, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::{Tool, ToolContext, ToolSchema};

pub struct EmailTool;

#[async_trait]
impl Tool for EmailTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "email",
            description: "Send and receive emails via SMTP/IMAP. Supports sending with attachments, reading inbox, searching emails, and parsing attachments. Configure SMTP/IMAP credentials in the agent config or pass them as parameters.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["send", "list", "read", "search"],
                        "description": "Action: send an email, list inbox, read a specific email, or search emails"
                    },
                    "smtp_host": {
                        "type": "string",
                        "description": "(send) SMTP server host, e.g. 'smtp.gmail.com'"
                    },
                    "smtp_port": {
                        "type": "integer",
                        "description": "(send) SMTP port, default 587 (STARTTLS) or 465 (SSL)"
                    },
                    "imap_host": {
                        "type": "string",
                        "description": "(list/read/search) IMAP server host, e.g. 'imap.gmail.com'"
                    },
                    "imap_port": {
                        "type": "integer",
                        "description": "(list/read/search) IMAP port, default 993 (SSL)"
                    },
                    "username": {
                        "type": "string",
                        "description": "Email account username (usually the email address)"
                    },
                    "password": {
                        "type": "string",
                        "description": "Email account password or app-specific password"
                    },
                    "from": {
                        "type": "string",
                        "description": "(send) Sender email address"
                    },
                    "to": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "(send) Recipient email addresses"
                    },
                    "cc": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "(send) CC recipients"
                    },
                    "subject": {
                        "type": "string",
                        "description": "(send) Email subject"
                    },
                    "body": {
                        "type": "string",
                        "description": "(send) Email body (plain text)"
                    },
                    "html_body": {
                        "type": "string",
                        "description": "(send) Email body (HTML). If both body and html_body are provided, sends as multipart."
                    },
                    "attachments": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "(send) File paths to attach"
                    },
                    "folder": {
                        "type": "string",
                        "description": "(list/read/search) IMAP folder, default 'INBOX'"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "(list) Max emails to return, default 10"
                    },
                    "uid": {
                        "type": "integer",
                        "description": "(read) Email UID to read"
                    },
                    "query": {
                        "type": "string",
                        "description": "(search) Search query. Supports IMAP search syntax: 'FROM \"user@example.com\"', 'SUBJECT \"hello\"', 'SINCE 01-Jan-2024', 'UNSEEN', etc."
                    },
                    "save_attachments_to": {
                        "type": "string",
                        "description": "(read) Directory to save attachments to"
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
            "send" => {
                if params.get("to").and_then(|v| v.as_array()).map(|a| a.is_empty()).unwrap_or(true) {
                    return Err(Error::Validation("send requires 'to' (non-empty array of recipients)".to_string()));
                }
                if params.get("subject").and_then(|v| v.as_str()).is_none() {
                    return Err(Error::Validation("send requires 'subject'".to_string()));
                }
                let has_body = params.get("body").and_then(|v| v.as_str()).is_some();
                let has_html = params.get("html_body").and_then(|v| v.as_str()).is_some();
                if !has_body && !has_html {
                    return Err(Error::Validation("send requires 'body' or 'html_body'".to_string()));
                }
            }
            "list" => {}
            "read" => {
                if params.get("uid").and_then(|v| v.as_u64()).is_none() {
                    return Err(Error::Validation("read requires 'uid'".to_string()));
                }
            }
            "search" => {
                if params.get("query").and_then(|v| v.as_str()).is_none() {
                    return Err(Error::Validation("search requires 'query'".to_string()));
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
            "send" => action_send(&workspace, &params).await,
            "list" => action_list_emails(&workspace, &params).await,
            "read" => action_read_email(&workspace, &params).await,
            "search" => action_search_emails(&workspace, &params).await,
            _ => Err(Error::Tool(format!("Unknown action: {}", action))),
        }
    }
}

fn expand_path(path: &str, workspace: &std::path::Path) -> PathBuf {
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

async fn action_send(workspace: &Path, params: &Value) -> Result<Value> {
    let smtp_host = params.get("smtp_host").and_then(|v| v.as_str())
        .ok_or_else(|| Error::Validation("send requires 'smtp_host'".to_string()))?;
    let smtp_port = params.get("smtp_port").and_then(|v| v.as_u64()).unwrap_or(587) as u16;
    let username = params.get("username").and_then(|v| v.as_str())
        .ok_or_else(|| Error::Validation("send requires 'username'".to_string()))?;
    let password = params.get("password").and_then(|v| v.as_str())
        .ok_or_else(|| Error::Validation("send requires 'password'".to_string()))?;

    let from = params.get("from").and_then(|v| v.as_str()).unwrap_or(username);
    let to: Vec<&str> = params["to"].as_array().unwrap()
        .iter().filter_map(|v| v.as_str()).collect();
    let cc: Vec<&str> = params.get("cc").and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    let subject = params["subject"].as_str().unwrap();
    let body = params.get("body").and_then(|v| v.as_str()).unwrap_or("");
    let html_body = params.get("html_body").and_then(|v| v.as_str());

    // Build email using lettre
    use lettre::message::{header, Mailbox, MultiPart, SinglePart, Attachment};
    use lettre::{Message, SmtpTransport, Transport};
    use lettre::transport::smtp::authentication::Credentials;

    let from_mailbox: Mailbox = from.parse()
        .map_err(|e| Error::Tool(format!("Invalid 'from' address '{}': {}", from, e)))?;

    let mut builder = Message::builder()
        .from(from_mailbox)
        .subject(subject);

    for addr in &to {
        let mb: Mailbox = addr.parse()
            .map_err(|e| Error::Tool(format!("Invalid 'to' address '{}': {}", addr, e)))?;
        builder = builder.to(mb);
    }
    for addr in &cc {
        let mb: Mailbox = addr.parse()
            .map_err(|e| Error::Tool(format!("Invalid 'cc' address '{}': {}", addr, e)))?;
        builder = builder.cc(mb);
    }

    // Collect attachments
    let attachment_paths: Vec<PathBuf> = params.get("attachments")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).map(|p| expand_path(p, workspace)).collect())
        .unwrap_or_default();

    let mut attachment_parts: Vec<SinglePart> = Vec::new();
    for att_path in &attachment_paths {
        if !att_path.exists() {
            return Err(Error::NotFound(format!("Attachment not found: {}", att_path.display())));
        }
        let filename = att_path.file_name().unwrap_or_default().to_string_lossy().to_string();
        let file_body = tokio::fs::read(att_path).await?;
        let content_type = match att_path.extension().and_then(|e| e.to_str()).unwrap_or("") {
            "pdf" => header::ContentType::parse("application/pdf").unwrap(),
            "png" => header::ContentType::parse("image/png").unwrap(),
            "jpg" | "jpeg" => header::ContentType::parse("image/jpeg").unwrap(),
            "gif" => header::ContentType::parse("image/gif").unwrap(),
            "zip" => header::ContentType::parse("application/zip").unwrap(),
            "csv" => header::ContentType::parse("text/csv").unwrap(),
            "xlsx" => header::ContentType::parse("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet").unwrap(),
            "docx" => header::ContentType::parse("application/vnd.openxmlformats-officedocument.wordprocessingml.document").unwrap(),
            _ => header::ContentType::parse("application/octet-stream").unwrap(),
        };
        attachment_parts.push(
            Attachment::new(filename).body(file_body, content_type)
        );
    }

    // Build message body
    let email = if attachment_parts.is_empty() {
        if let Some(html) = html_body {
            if !body.is_empty() {
                builder.multipart(
                    MultiPart::alternative()
                        .singlepart(SinglePart::plain(body.to_string()))
                        .singlepart(SinglePart::html(html.to_string()))
                ).map_err(|e| Error::Tool(format!("Failed to build email: {}", e)))?
            } else {
                builder.header(header::ContentType::TEXT_HTML)
                    .body(html.to_string())
                    .map_err(|e| Error::Tool(format!("Failed to build email: {}", e)))?
            }
        } else {
            builder.header(header::ContentType::TEXT_PLAIN)
                .body(body.to_string())
                .map_err(|e| Error::Tool(format!("Failed to build email: {}", e)))?
        }
    } else {
        // With attachments: use mixed multipart
        let text_part = if let Some(html) = html_body {
            MultiPart::alternative()
                .singlepart(SinglePart::plain(body.to_string()))
                .singlepart(SinglePart::html(html.to_string()))
        } else {
            MultiPart::mixed()
                .singlepart(SinglePart::plain(body.to_string()))
        };

        let mut mixed = MultiPart::mixed().multipart(text_part);
        for att in attachment_parts {
            mixed = mixed.singlepart(att);
        }

        builder.multipart(mixed)
            .map_err(|e| Error::Tool(format!("Failed to build email: {}", e)))?
    };

    // Send via SMTP
    let creds = Credentials::new(username.to_string(), password.to_string());

    let mailer = if smtp_port == 465 {
        SmtpTransport::relay(smtp_host)
            .map_err(|e| Error::Tool(format!("SMTP relay error: {}", e)))?
            .credentials(creds)
            .port(smtp_port)
            .build()
    } else {
        SmtpTransport::starttls_relay(smtp_host)
            .map_err(|e| Error::Tool(format!("SMTP STARTTLS error: {}", e)))?
            .credentials(creds)
            .port(smtp_port)
            .build()
    };

    mailer.send(&email)
        .map_err(|e| Error::Tool(format!("Failed to send email: {}", e)))?;

    Ok(json!({
        "status": "sent",
        "from": from,
        "to": to,
        "cc": cc,
        "subject": subject,
        "attachments": attachment_paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>()
    }))
}

async fn connect_imap(params: &Value) -> Result<async_imap::Session<tokio_util::compat::Compat<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>>> {
    use tokio_util::compat::TokioAsyncReadCompatExt;
    use tokio_rustls::TlsConnector;
    use tokio_rustls::rustls::{ClientConfig, RootCertStore, Certificate};
    use tokio_rustls::rustls::ServerName;
    use std::sync::Arc;

    let host = params.get("imap_host").and_then(|v| v.as_str())
        .ok_or_else(|| Error::Validation("IMAP requires 'imap_host'".to_string()))?;
    let port = params.get("imap_port").and_then(|v| v.as_u64()).unwrap_or(993) as u16;
    let username = params.get("username").and_then(|v| v.as_str())
        .ok_or_else(|| Error::Validation("IMAP requires 'username'".to_string()))?;
    let password = params.get("password").and_then(|v| v.as_str())
        .ok_or_else(|| Error::Validation("IMAP requires 'password'".to_string()))?;

    let mut root_store = RootCertStore::empty();
    let native_certs = rustls_native_certs::load_native_certs()
        .map_err(|e| Error::Tool(format!("Failed to load native certs: {}", e)))?;
    for cert in native_certs {
        let _ = root_store.add(&Certificate(cert.as_ref().to_vec()));
    }

    let config = ClientConfig::builder()
        .with_safe_defaults()
        .with_root_certificates(root_store)
        .with_no_client_auth();
    let connector = TlsConnector::from(Arc::new(config));

    let server_name = ServerName::try_from(host)
        .map_err(|e| Error::Tool(format!("Invalid IMAP hostname '{}': {}", host, e)))?;

    let tcp = tokio::net::TcpStream::connect((host, port)).await
        .map_err(|e| Error::Tool(format!("IMAP TCP connect error: {}", e)))?;
    let tls_stream = connector.connect(server_name, tcp).await
        .map_err(|e| Error::Tool(format!("IMAP TLS error: {}", e)))?;

    let client = async_imap::Client::new(tls_stream.compat());

    let session = client.login(username, password).await
        .map_err(|e| Error::Tool(format!("IMAP login error: {}", e.0)))?;

    Ok(session)
}

async fn action_list_emails(_workspace: &PathBuf, params: &Value) -> Result<Value> {
    let folder = params.get("folder").and_then(|v| v.as_str()).unwrap_or("INBOX");
    let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;

    let mut session = connect_imap(params).await?;
    let mailbox = session.select(folder).await
        .map_err(|e| Error::Tool(format!("Failed to select folder '{}': {}", folder, e)))?;

    let total = mailbox.exists as usize;
    if total == 0 {
        let _ = session.logout().await;
        return Ok(json!({
            "folder": folder,
            "total": 0,
            "emails": []
        }));
    }

    let start = if total > limit { total - limit + 1 } else { 1 };
    let range = format!("{}:{}", start, total);

    use futures::TryStreamExt;
    let messages: Vec<_> = session.fetch(&range, "(UID ENVELOPE FLAGS)").await
        .map_err(|e| Error::Tool(format!("IMAP fetch error: {}", e)))?
        .try_collect().await
        .map_err(|e| Error::Tool(format!("IMAP fetch stream error: {}", e)))?;

    let mut emails = Vec::new();
    for msg in &messages {
        let uid = msg.uid.unwrap_or(0);
        let flags: Vec<String> = msg.flags().map(|f| format!("{:?}", f)).collect();

        let mut email_info = json!({
            "uid": uid,
            "flags": flags
        });

        if let Some(envelope) = msg.envelope() {
            if let Some(subject) = &envelope.subject {
                let subject_str = String::from_utf8_lossy(subject).to_string();
                email_info["subject"] = json!(decode_mime_header(&subject_str));
            }
            if let Some(from) = &envelope.from {
                let addrs: Vec<String> = from.iter().map(|a| format_address(a)).collect();
                email_info["from"] = json!(addrs);
            }
            if let Some(to) = &envelope.to {
                let addrs: Vec<String> = to.iter().map(|a| format_address(a)).collect();
                email_info["to"] = json!(addrs);
            }
            if let Some(date) = &envelope.date {
                email_info["date"] = json!(String::from_utf8_lossy(date).to_string());
            }
        }

        emails.push(email_info);
    }

    // Reverse so newest first
    emails.reverse();

    let _ = session.logout().await;

    Ok(json!({
        "folder": folder,
        "total": total,
        "returned": emails.len(),
        "emails": emails
    }))
}

async fn action_read_email(workspace: &Path, params: &Value) -> Result<Value> {
    let folder = params.get("folder").and_then(|v| v.as_str()).unwrap_or("INBOX");
    let uid = params["uid"].as_u64().unwrap() as u32;
    let save_dir = params.get("save_attachments_to").and_then(|v| v.as_str());

    let mut session = connect_imap(params).await?;
    session.select(folder).await
        .map_err(|e| Error::Tool(format!("Failed to select folder: {}", e)))?;

    use futures::TryStreamExt;
    let messages: Vec<_> = session.uid_fetch(format!("{}", uid), "(UID ENVELOPE BODY[] FLAGS)").await
        .map_err(|e| Error::Tool(format!("IMAP uid_fetch error: {}", e)))?
        .try_collect().await
        .map_err(|e| Error::Tool(format!("IMAP uid_fetch stream error: {}", e)))?;

    let msg = messages.first()
        .ok_or_else(|| Error::NotFound(format!("Email with UID {} not found", uid)))?;

    let mut result = json!({
        "uid": uid,
        "folder": folder
    });

    // Parse envelope
    if let Some(envelope) = msg.envelope() {
        if let Some(subject) = &envelope.subject {
            result["subject"] = json!(decode_mime_header(&String::from_utf8_lossy(subject)));
        }
        if let Some(from) = &envelope.from {
            result["from"] = json!(from.iter().map(format_address).collect::<Vec<String>>());
        }
        if let Some(to) = &envelope.to {
            result["to"] = json!(to.iter().map(format_address).collect::<Vec<String>>());
        }
        if let Some(cc) = &envelope.cc {
            result["cc"] = json!(cc.iter().map(format_address).collect::<Vec<String>>());
        }
        if let Some(date) = &envelope.date {
            result["date"] = json!(String::from_utf8_lossy(date).to_string());
        }
    }

    // Parse body
    if let Some(body_bytes) = msg.body() {
        let raw = String::from_utf8_lossy(body_bytes).to_string();

        let (text_content, attachments_info) = parse_email_body(&raw, workspace, save_dir)?;

        result["body"] = json!(text_content);
        if !attachments_info.is_empty() {
            result["attachments"] = json!(attachments_info);
        }
    }

    let flags: Vec<String> = msg.flags().map(|f| format!("{:?}", f)).collect();
    result["flags"] = json!(flags);

    let _ = session.logout().await;

    Ok(result)
}

async fn action_search_emails(_workspace: &PathBuf, params: &Value) -> Result<Value> {
    let folder = params.get("folder").and_then(|v| v.as_str()).unwrap_or("INBOX");
    let query = params["query"].as_str().unwrap();
    let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;

    let mut session = connect_imap(params).await?;
    session.select(folder).await
        .map_err(|e| Error::Tool(format!("Failed to select folder: {}", e)))?;

    let uids = session.uid_search(query).await
        .map_err(|e| Error::Tool(format!("IMAP search error: {}", e)))?;

    let uid_list: Vec<u32> = uids.iter().copied().collect();
    let total = uid_list.len();

    // Take the last N (most recent)
    let selected: Vec<u32> = if uid_list.len() > limit {
        uid_list[uid_list.len() - limit..].to_vec()
    } else {
        uid_list
    };

    let mut emails = Vec::new();
    if !selected.is_empty() {
        let uid_range = selected.iter().map(|u| u.to_string()).collect::<Vec<_>>().join(",");
        use futures::TryStreamExt;
        let messages: Vec<_> = session.uid_fetch(&uid_range, "(UID ENVELOPE FLAGS)").await
            .map_err(|e| Error::Tool(format!("IMAP fetch error: {}", e)))?
            .try_collect().await
            .map_err(|e| Error::Tool(format!("IMAP fetch stream error: {}", e)))?;

        for msg in &messages {
            let uid = msg.uid.unwrap_or(0);
            let flags: Vec<String> = msg.flags().map(|f| format!("{:?}", f)).collect();
            let mut email_info = json!({"uid": uid, "flags": flags});

            if let Some(envelope) = msg.envelope() {
                if let Some(subject) = &envelope.subject {
                    email_info["subject"] = json!(decode_mime_header(&String::from_utf8_lossy(subject)));
                }
                if let Some(from) = &envelope.from {
                    email_info["from"] = json!(from.iter().map(format_address).collect::<Vec<String>>());
                }
                if let Some(date) = &envelope.date {
                    email_info["date"] = json!(String::from_utf8_lossy(date).to_string());
                }
            }
            emails.push(email_info);
        }
    }

    emails.reverse();

    let _ = session.logout().await;

    Ok(json!({
        "folder": folder,
        "query": query,
        "total_matches": total,
        "returned": emails.len(),
        "emails": emails
    }))
}

fn format_address(addr: &async_imap::imap_proto::types::Address<'_>) -> String {
    let name = addr.name.as_ref()
        .map(|n| decode_mime_header(&String::from_utf8_lossy(n)))
        .unwrap_or_default();
    let mailbox = addr.mailbox.as_ref()
        .map(|m| String::from_utf8_lossy(m).to_string())
        .unwrap_or_default();
    let host = addr.host.as_ref()
        .map(|h| String::from_utf8_lossy(h).to_string())
        .unwrap_or_default();

    let email = format!("{}@{}", mailbox, host);
    if name.is_empty() {
        email
    } else {
        format!("{} <{}>", name, email)
    }
}

fn decode_mime_header(s: &str) -> String {
    // Basic MIME encoded-word decoding (=?charset?encoding?text?=)
    // For full support we'd use the `charset` or `mailparse` crate
    if !s.contains("=?") {
        return s.to_string();
    }

    let mut result = s.to_string();
    // Simple regex-free approach: find =?...?= patterns
    while let Some(start) = result.find("=?") {
        if let Some(end) = result[start + 2..].find("?=") {
            let encoded = &result[start..start + 2 + end + 2];
            let parts: Vec<&str> = encoded[2..encoded.len() - 2].splitn(3, '?').collect();
            if parts.len() == 3 {
                let _charset = parts[0];
                let encoding = parts[1].to_uppercase();
                let text = parts[2];

                let decoded = match encoding.as_str() {
                    "B" => {
                        use base64::Engine;
                        base64::engine::general_purpose::STANDARD.decode(text)
                            .ok()
                            .map(|bytes| String::from_utf8_lossy(&bytes).to_string())
                            .unwrap_or_else(|| text.to_string())
                    }
                    "Q" => {
                        // Quoted-printable
                        text.replace('_', " ")
                            .replace("=20", " ")
                    }
                    _ => text.to_string(),
                };
                result = format!("{}{}{}", &result[..start], decoded, &result[start + 2 + end + 2..]);
            } else {
                break;
            }
        } else {
            break;
        }
    }
    result
}

fn parse_email_body(
    raw: &str,
    workspace: &Path,
    save_dir: Option<&str>,
) -> Result<(String, Vec<Value>)> {
    // Simple MIME body extraction
    // Look for Content-Type boundary or just return the raw text
    let mut text_content = String::new();
    let mut attachments = Vec::new();

    // Check if it's multipart
    if let Some(boundary_line) = raw.lines().find(|l| l.contains("boundary=")) {
        let boundary = extract_boundary(boundary_line);
        if !boundary.is_empty() {
            let parts: Vec<&str> = raw.split(&format!("--{}", boundary)).collect();
            for part in &parts[1..] {
                if part.starts_with("--") {
                    continue; // End boundary
                }

                let (headers, body) = split_headers_body(part);
                let content_type = get_header(&headers, "Content-Type").unwrap_or_default().to_lowercase();
                let disposition = get_header(&headers, "Content-Disposition").unwrap_or_default();

                if disposition.contains("attachment") || content_type.starts_with("application/") {
                    // Attachment
                    let filename = extract_filename(disposition)
                        .or_else(|| extract_param(&content_type, "name"))
                        .unwrap_or_else(|| "attachment".to_string());

                    let mut att_info = json!({
                        "filename": filename,
                        "content_type": content_type
                    });

                    if let Some(dir) = save_dir {
                        let save_path = expand_path(dir, workspace).join(&filename);
                        if let Some(parent) = save_path.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        // Decode and save
                        let encoding = get_header(&headers, "Content-Transfer-Encoding").unwrap_or_default();
                        let decoded = decode_body(body.trim(), encoding);
                        let _ = std::fs::write(&save_path, &decoded);
                        att_info["saved_to"] = json!(save_path.display().to_string());
                        att_info["size"] = json!(decoded.len());
                    }

                    attachments.push(att_info);
                } else if content_type.contains("text/plain") {
                    let encoding = get_header(&headers, "Content-Transfer-Encoding").unwrap_or_default();
                    let decoded = decode_body(body.trim(), encoding);
                    text_content = String::from_utf8_lossy(&decoded).to_string();
                } else if content_type.contains("text/html") && text_content.is_empty() {
                    let encoding = get_header(&headers, "Content-Transfer-Encoding").unwrap_or_default();
                    let decoded = decode_body(body.trim(), encoding);
                    let html = String::from_utf8_lossy(&decoded).to_string();
                    // Strip HTML tags for plain text
                    text_content = strip_html_tags(&html);
                }
            }
        }
    }

    if text_content.is_empty() {
        // Not multipart, just return the body after headers
        let (_, body) = split_headers_body(raw);
        text_content = body.trim().to_string();
    }

    // Truncate very long emails
    if text_content.len() > 50000 {
        let mut end = 50000;
        while end > 0 && !text_content.is_char_boundary(end) {
            end -= 1;
        }
        text_content = format!("{}... (truncated)", &text_content[..end]);
    }

    Ok((text_content, attachments))
}

fn extract_boundary(line: &str) -> String {
    if let Some(idx) = line.find("boundary=") {
        let rest = &line[idx + 9..];
        let boundary = rest.trim().trim_matches('"').trim_matches('\'');
        // Take until whitespace or semicolon
        boundary.split(|c: char| c == ';' || c.is_whitespace())
            .next()
            .unwrap_or("")
            .trim_matches('"')
            .to_string()
    } else {
        String::new()
    }
}

fn split_headers_body(part: &str) -> (String, String) {
    if let Some(idx) = part.find("\r\n\r\n") {
        (part[..idx].to_string(), part[idx + 4..].to_string())
    } else if let Some(idx) = part.find("\n\n") {
        (part[..idx].to_string(), part[idx + 2..].to_string())
    } else {
        (String::new(), part.to_string())
    }
}

fn get_header<'a>(headers: &'a str, name: &str) -> Option<&'a str> {
    let lower_name = name.to_lowercase();
    for line in headers.lines() {
        let lower_line = line.to_lowercase();
        if lower_line.starts_with(&format!("{}:", lower_name)) {
            return Some(line[name.len() + 1..].trim());
        }
    }
    None
}

fn extract_filename(disposition: &str) -> Option<String> {
    extract_param(disposition, "filename")
}

fn extract_param(header: &str, param: &str) -> Option<String> {
    let lower = header.to_lowercase();
    let search = format!("{}=", param);
    if let Some(idx) = lower.find(&search) {
        let rest = &header[idx + search.len()..];
        let value = rest.trim().trim_matches('"').trim_matches('\'');
        let end = value.find(';').unwrap_or(value.len());
        Some(value[..end].trim_matches('"').to_string())
    } else {
        None
    }
}

fn decode_body(body: &str, encoding: &str) -> Vec<u8> {
    match encoding.to_lowercase().as_str() {
        "base64" => {
            use base64::Engine;
            let cleaned: String = body.chars().filter(|c| !c.is_whitespace()).collect();
            base64::engine::general_purpose::STANDARD.decode(&cleaned)
                .unwrap_or_else(|_| body.as_bytes().to_vec())
        }
        "quoted-printable" => {
            decode_quoted_printable(body)
        }
        _ => body.as_bytes().to_vec(),
    }
}

fn decode_quoted_printable(input: &str) -> Vec<u8> {
    let mut result = Vec::new();
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '=' {
            if chars.peek() == Some(&'\r') || chars.peek() == Some(&'\n') {
                // Soft line break
                chars.next();
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
            } else {
                let h1 = chars.next().unwrap_or('0');
                let h2 = chars.next().unwrap_or('0');
                let hex = format!("{}{}", h1, h2);
                if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                    result.push(byte);
                }
            }
        } else {
            let mut buf = [0u8; 4];
            let encoded = c.encode_utf8(&mut buf);
            result.extend_from_slice(encoded.as_bytes());
        }
    }
    result
}

fn strip_html_tags(html: &str) -> String {
    let mut result = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        if c == '<' {
            in_tag = true;
        } else if c == '>' {
            in_tag = false;
        } else if !in_tag {
            result.push(c);
        }
    }
    // Collapse whitespace
    let mut prev_space = false;
    let collapsed: String = result.chars().filter(|c| {
        if c.is_whitespace() {
            if prev_space { return false; }
            prev_space = true;
        } else {
            prev_space = false;
        }
        true
    }).collect();
    collapsed.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema() {
        let tool = EmailTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "email");
    }

    #[test]
    fn test_validate_send() {
        let tool = EmailTool;
        assert!(tool.validate(&json!({
            "action": "send",
            "to": ["test@example.com"],
            "subject": "Test",
            "body": "Hello"
        })).is_ok());
        assert!(tool.validate(&json!({"action": "send", "to": [], "subject": "Test", "body": "Hi"})).is_err());
        assert!(tool.validate(&json!({"action": "send", "to": ["a@b.com"], "subject": "Test"})).is_err());
    }

    #[test]
    fn test_validate_read() {
        let tool = EmailTool;
        assert!(tool.validate(&json!({"action": "read", "uid": 123})).is_ok());
        assert!(tool.validate(&json!({"action": "read"})).is_err());
    }

    #[test]
    fn test_decode_mime_header() {
        assert_eq!(decode_mime_header("Hello World"), "Hello World");
        // Base64 encoded
        let encoded = "=?UTF-8?B?SGVsbG8=?=";
        assert_eq!(decode_mime_header(encoded), "Hello");
    }

    #[test]
    fn test_extract_boundary() {
        assert_eq!(
            extract_boundary("Content-Type: multipart/mixed; boundary=\"abc123\""),
            "abc123"
        );
    }

    #[test]
    fn test_strip_html_tags() {
        assert_eq!(strip_html_tags("<p>Hello <b>World</b></p>"), "Hello World");
    }

    #[test]
    fn test_decode_quoted_printable() {
        let input = "Hello=20World";
        let decoded = decode_quoted_printable(input);
        assert_eq!(String::from_utf8_lossy(&decoded), "Hello World");
    }
}
