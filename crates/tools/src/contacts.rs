use async_trait::async_trait;
use blockcell_core::{Error, Result};
use reqwest::Client;
use serde_json::{json, Value};
use tracing::debug;

use crate::{Tool, ToolContext, ToolSchema};

/// Contacts management tool.
///
/// Supported sources:
/// - **macos**: macOS Contacts via Python/pyobjc bridge (Contacts.framework)
/// - **google**: Google People API (contacts.readonly / contacts scopes)
/// - **carddav**: Generic CardDAV server (e.g. Nextcloud, iCloud)
pub struct ContactsTool;

#[async_trait]
impl Tool for ContactsTool {
    fn schema(&self) -> ToolSchema {
        let mut props = serde_json::Map::new();
        props.insert("source".into(), json!({"type": "string", "enum": ["macos", "google", "carddav"], "description": "Contacts source. 'macos' for local macOS Contacts, 'google' for Google People API, 'carddav' for CardDAV server."}));
        props.insert("action".into(), json!({"type": "string", "description": "Action: list|search|get|create|update|delete|export|groups"}));
        props.insert("query".into(), json!({"type": "string", "description": "(search) Search query — matches name, email, phone, organization"}));
        props.insert("contact_id".into(), json!({"type": "string", "description": "(get/update/delete) Contact identifier"}));
        props.insert("name".into(), json!({"type": "string", "description": "(create/update) Full name or structured {first, last, middle}"}));
        props.insert("email".into(), json!({"type": "string", "description": "(create/update) Email address"}));
        props.insert("phone".into(), json!({"type": "string", "description": "(create/update) Phone number"}));
        props.insert("organization".into(), json!({"type": "string", "description": "(create/update) Organization/company name"}));
        props.insert("title".into(), json!({"type": "string", "description": "(create/update) Job title"}));
        props.insert("address".into(), json!({"type": "string", "description": "(create/update) Postal address"}));
        props.insert("notes".into(), json!({"type": "string", "description": "(create/update) Notes field"}));
        props.insert("birthday".into(), json!({"type": "string", "description": "(create/update) Birthday in YYYY-MM-DD format"}));
        props.insert("group".into(), json!({"type": "string", "description": "(list/create) Filter by group name or add to group"}));
        props.insert("output_path".into(), json!({"type": "string", "description": "(export) Output file path for vCard export (.vcf)"}));
        props.insert("max_results".into(), json!({"type": "integer", "description": "(list/search) Max results. Default: 50"}));
        props.insert("api_token".into(), json!({"type": "string", "description": "(google/carddav) OAuth2 access token or password"}));
        props.insert("api_base".into(), json!({"type": "string", "description": "(carddav) CardDAV server URL"}));
        props.insert("username".into(), json!({"type": "string", "description": "(carddav) Username for CardDAV authentication"}));

        ToolSchema {
            name: "contacts",
            description: "Manage contacts. List, search, create, update, delete contacts and export to vCard. Sources: 'macos' (local macOS Contacts app via Python bridge), 'google' (Google People API, requires GOOGLE_CONTACTS_TOKEN), 'carddav' (any CardDAV server).",
            parameters: json!({
                "type": "object",
                "properties": Value::Object(props),
                "required": ["source", "action"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let source = params.get("source").and_then(|v| v.as_str()).unwrap_or("");
        if !["macos", "google", "carddav"].contains(&source) {
            return Err(Error::Tool("source must be 'macos', 'google', or 'carddav'".into()));
        }
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let valid = ["list", "search", "get", "create", "update", "delete", "export", "groups"];
        if !valid.contains(&action) {
            return Err(Error::Tool(format!("Invalid action '{}'. Valid: {}", action, valid.join(", "))));
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let source = params["source"].as_str().unwrap_or("macos");
        let action = params["action"].as_str().unwrap_or("");

        debug!(source = source, action = action, "contacts execute");

        match source {
            "macos" => execute_macos(action, &params, &ctx).await,
            "google" => execute_google(action, &params, &ctx).await,
            "carddav" => execute_carddav(action, &params, &ctx).await,
            _ => Err(Error::Tool(format!("Unknown source: {}", source))),
        }
    }
}

// ─── macOS Contacts via Python/pyobjc ───────────────────────────────────────

async fn execute_macos(action: &str, params: &Value, ctx: &ToolContext) -> Result<Value> {
    let script = build_macos_script(action, params, &ctx.workspace)?;
    let output = tokio::process::Command::new("python3")
        .arg("-c")
        .arg(&script)
        .output()
        .await
        .map_err(|e| Error::Tool(format!("Failed to run Python: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // If pyobjc not available, fall back to AppleScript
        if stderr.contains("No module named") || stderr.contains("ImportError") {
            return execute_macos_applescript(action, params, ctx).await;
        }
        return Err(Error::Tool(format!("macOS contacts error: {}", stderr)));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&stdout)
        .map_err(|e| Error::Tool(format!("Failed to parse contacts output: {} — raw: {}", e, &stdout[..stdout.len().min(200)])))
}

fn build_macos_script(action: &str, params: &Value, workspace: &std::path::Path) -> Result<String> {
    let max_results = params.get("max_results").and_then(|v| v.as_u64()).unwrap_or(50);

    let script = match action {
        "list" => {
            let _group_filter = params.get("group").and_then(|v| v.as_str()).unwrap_or("");
            format!(r#"
import Contacts, json
store = Contacts.CNContactStore.alloc().init()
keys = [
    Contacts.CNContactGivenNameKey, Contacts.CNContactFamilyNameKey,
    Contacts.CNContactEmailAddressesKey, Contacts.CNContactPhoneNumbersKey,
    Contacts.CNContactOrganizationNameKey, Contacts.CNContactJobTitleKey,
    Contacts.CNContactIdentifierKey, Contacts.CNContactNoteKey,
    Contacts.CNContactBirthdayKey,
]
req = Contacts.CNContactFetchRequest.alloc().initWithKeysToFetch_(keys)
results = []
def handler(contact, stop):
    if len(results) >= {max}:
        stop[0] = True
        return
    name = f"{{contact.givenName()}} {{contact.familyName()}}".strip()
    emails = [str(e.value()) for e in (contact.emailAddresses() or [])]
    phones = [str(p.value().stringValue()) for p in (contact.phoneNumbers() or [])]
    org = str(contact.organizationName() or "")
    results.append({{"id": str(contact.identifier()), "name": name, "emails": emails, "phones": phones, "organization": org}})
store.enumerateContactsWithFetchRequest_error_usingBlock_(req, None, handler)
print(json.dumps({{"contacts": results, "count": len(results)}}))
"#, max = max_results)
        }
        "search" => {
            let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let escaped = query.replace('\\', "\\\\").replace('"', "\\\"");
            format!(r#"
import Contacts, json
store = Contacts.CNContactStore.alloc().init()
keys = [
    Contacts.CNContactGivenNameKey, Contacts.CNContactFamilyNameKey,
    Contacts.CNContactEmailAddressesKey, Contacts.CNContactPhoneNumbersKey,
    Contacts.CNContactOrganizationNameKey, Contacts.CNContactJobTitleKey,
    Contacts.CNContactIdentifierKey,
]
predicate = Contacts.CNContact.predicateForContactsMatchingName_("{query}")
contacts, err = store.unifiedContactsMatchingPredicate_keysToFetch_error_(predicate, keys, None)
results = []
for c in (contacts or [])[:{max}]:
    name = f"{{c.givenName()}} {{c.familyName()}}".strip()
    emails = [str(e.value()) for e in (c.emailAddresses() or [])]
    phones = [str(p.value().stringValue()) for p in (c.phoneNumbers() or [])]
    org = str(c.organizationName() or "")
    results.append({{"id": str(c.identifier()), "name": name, "emails": emails, "phones": phones, "organization": org}})
print(json.dumps({{"contacts": results, "count": len(results), "query": "{query}"}}))
"#, query = escaped, max = max_results)
        }
        "get" => {
            let contact_id = params.get("contact_id").and_then(|v| v.as_str())
                .ok_or_else(|| Error::Tool("contact_id is required for get".into()))?;
            let escaped_id = contact_id.replace('\\', "\\\\").replace('"', "\\\"");
            format!(r#"
import Contacts, json
store = Contacts.CNContactStore.alloc().init()
keys = [
    Contacts.CNContactGivenNameKey, Contacts.CNContactFamilyNameKey,
    Contacts.CNContactEmailAddressesKey, Contacts.CNContactPhoneNumbersKey,
    Contacts.CNContactOrganizationNameKey, Contacts.CNContactJobTitleKey,
    Contacts.CNContactIdentifierKey, Contacts.CNContactNoteKey,
    Contacts.CNContactBirthdayKey, Contacts.CNContactPostalAddressesKey,
]
predicate = Contacts.CNContact.predicateForContactsWithIdentifiers_(["{id}"])
contacts, err = store.unifiedContactsMatchingPredicate_keysToFetch_error_(predicate, keys, None)
if contacts and len(contacts) > 0:
    c = contacts[0]
    name = f"{{c.givenName()}} {{c.familyName()}}".strip()
    emails = [str(e.value()) for e in (c.emailAddresses() or [])]
    phones = [str(p.value().stringValue()) for p in (c.phoneNumbers() or [])]
    org = str(c.organizationName() or "")
    title = str(c.jobTitle() or "")
    note = str(c.note() or "")
    bday = ""
    if c.birthday():
        bday = f"{{c.birthday().year()}}-{{c.birthday().month():02d}}-{{c.birthday().day():02d}}"
    addrs = []
    for a in (c.postalAddresses() or []):
        v = a.value()
        addrs.append(Contacts.CNPostalAddressFormatter.stringFromPostalAddress_style_(v, 0))
    print(json.dumps({{"id": str(c.identifier()), "name": name, "first_name": str(c.givenName()), "last_name": str(c.familyName()), "emails": emails, "phones": phones, "organization": org, "title": title, "notes": note, "birthday": bday, "addresses": addrs}}))
else:
    print(json.dumps({{"error": "Contact not found"}}))
"#, id = escaped_id)
        }
        "create" | "update" | "delete" => {
            // These require write access — use AppleScript fallback for safety
            return Err(Error::Tool(format!(
                "macOS contacts '{}' action requires Contacts write permission. Use source='google' or source='carddav' for write operations, or grant Contacts access to Terminal/Python.",
                action
            )));
        }
        "export" => {
            let output_path = params.get("output_path").and_then(|v| v.as_str())
                .unwrap_or("");
            let out = if output_path.is_empty() {
                let ts = chrono::Utc::now().format("%Y%m%d_%H%M%S");
                workspace.join(format!("contacts_export_{}.vcf", ts)).to_string_lossy().to_string()
            } else {
                output_path.to_string()
            };
            let escaped_out = out.replace('\\', "\\\\").replace('"', "\\\"");
            format!(r#"
import Contacts, json
store = Contacts.CNContactStore.alloc().init()
keys = [
    Contacts.CNContactGivenNameKey, Contacts.CNContactFamilyNameKey,
    Contacts.CNContactEmailAddressesKey, Contacts.CNContactPhoneNumbersKey,
    Contacts.CNContactOrganizationNameKey, Contacts.CNContactIdentifierKey,
    Contacts.CNContactVCardSerialization,
]
req = Contacts.CNContactFetchRequest.alloc().initWithKeysToFetch_([
    Contacts.CNContactVCardSerialization.descriptorForRequiredKeysForVCard(),
])
contacts = []
def handler(contact, stop):
    contacts.append(contact)
store.enumerateContactsWithFetchRequest_error_usingBlock_(req, None, handler)
data = Contacts.CNContactVCardSerialization.dataWithContacts_error_(contacts, None)[0]
with open("{path}", "wb") as f:
    f.write(data.bytes().tobytes())
print(json.dumps({{"exported": len(contacts), "path": "{path}"}}))
"#, path = escaped_out)
        }
        "groups" => {
            r#"
import Contacts, json
store = Contacts.CNContactStore.alloc().init()
groups, err = store.groupsMatchingPredicate_error_(None, None)
results = []
for g in (groups or []):
    results.append({"id": str(g.identifier()), "name": str(g.name())})
print(json.dumps({"groups": results, "count": len(results)}))
"#.to_string()
        }
        _ => return Err(Error::Tool(format!("Unknown action: {}", action))),
    };

    Ok(script)
}

/// Fallback: use AppleScript for basic read operations when pyobjc is not available.
async fn execute_macos_applescript(action: &str, params: &Value, _ctx: &ToolContext) -> Result<Value> {
    let max_results = params.get("max_results").and_then(|v| v.as_u64()).unwrap_or(50) as usize;

    let script = match action {
        "list" => {
            format!(r#"
tell application "Contacts"
    set output to ""
    set pList to people
    set maxCount to {}
    set i to 0
    repeat with p in pList
        if i >= maxCount then exit repeat
        set pName to name of p
        set pEmails to ""
        try
            set pEmails to value of emails of p as text
        end try
        set pPhones to ""
        try
            set pPhones to value of phones of p as text
        end try
        set pOrg to ""
        try
            set pOrg to organization of p
        end try
        set output to output & pName & " | " & pEmails & " | " & pPhones & " | " & pOrg & linefeed
        set i to i + 1
    end repeat
    return output
end tell
"#, max_results)
        }
        "search" => {
            let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let escaped = query.replace('"', "\\\"");
            format!(r#"
tell application "Contacts"
    set output to ""
    set matches to (every person whose name contains "{}")
    repeat with p in matches
        set pName to name of p
        set pEmails to ""
        try
            set pEmails to value of emails of p as text
        end try
        set pPhones to ""
        try
            set pPhones to value of phones of p as text
        end try
        set output to output & pName & " | " & pEmails & " | " & pPhones & linefeed
    end repeat
    return output
end tell
"#, escaped)
        }
        _ => {
            return Err(Error::Tool(format!(
                "macOS contacts '{}' via AppleScript fallback is not supported. Install pyobjc: pip3 install pyobjc-framework-Contacts",
                action
            )));
        }
    };

    let output = tokio::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .await
        .map_err(|e| Error::Tool(format!("AppleScript failed: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Tool(format!("AppleScript error: {}", stderr)));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.trim().lines().collect();
    let contacts: Vec<Value> = lines.iter().map(|line| {
        let parts: Vec<&str> = line.splitn(4, " | ").collect();
        json!({
            "name": parts.first().unwrap_or(&""),
            "emails": parts.get(1).unwrap_or(&""),
            "phones": parts.get(2).unwrap_or(&""),
            "organization": parts.get(3).unwrap_or(&""),
        })
    }).collect();

    Ok(json!({"contacts": contacts, "count": contacts.len(), "source": "applescript_fallback"}))
}

// ─── Google People API ──────────────────────────────────────────────────────

fn resolve_google_token(params: &Value, ctx: &ToolContext) -> Result<String> {
    if let Some(t) = params.get("api_token").and_then(|v| v.as_str()) {
        if !t.is_empty() { return Ok(t.to_string()); }
    }
    if let Some(pc) = ctx.config.get_provider("google_contacts") {
        if !pc.api_key.is_empty() { return Ok(pc.api_key.clone()); }
    }
    if let Ok(val) = std::env::var("GOOGLE_CONTACTS_TOKEN") {
        if !val.is_empty() { return Ok(val); }
    }
    Err(Error::Tool("No Google Contacts token. Set via api_token, config providers.google_contacts.api_key, or GOOGLE_CONTACTS_TOKEN env.".into()))
}

async fn execute_google(action: &str, params: &Value, ctx: &ToolContext) -> Result<Value> {
    let token = resolve_google_token(params, ctx)?;
    let client = Client::new();
    let base = "https://people.googleapis.com/v1";

    match action {
        "list" => {
            let max_results = params.get("max_results").and_then(|v| v.as_u64()).unwrap_or(50);
            let url = format!("{}/people/me/connections", base);
            let resp = client.get(&url)
                .bearer_auth(&token)
                .query(&[
                    ("personFields", "names,emailAddresses,phoneNumbers,organizations,birthdays,addresses,biographies"),
                    ("pageSize", &max_results.to_string()),
                ])
                .send().await
                .map_err(|e| Error::Tool(format!("Google People API error: {}", e)))?;
            parse_google_people_response(resp).await
        }
        "search" => {
            let query = params.get("query").and_then(|v| v.as_str())
                .ok_or_else(|| Error::Tool("query is required for search".into()))?;
            let max_results = params.get("max_results").and_then(|v| v.as_u64()).unwrap_or(10);
            let url = format!("{}/people:searchContacts", base);
            let resp = client.get(&url)
                .bearer_auth(&token)
                .query(&[
                    ("query", query),
                    ("readMask", "names,emailAddresses,phoneNumbers,organizations"),
                    ("pageSize", &max_results.to_string()),
                ])
                .send().await
                .map_err(|e| Error::Tool(format!("Google People API error: {}", e)))?;
            parse_google_people_response(resp).await
        }
        "get" => {
            let contact_id = params.get("contact_id").and_then(|v| v.as_str())
                .ok_or_else(|| Error::Tool("contact_id is required for get".into()))?;
            let resource_name = if contact_id.starts_with("people/") {
                contact_id.to_string()
            } else {
                format!("people/{}", contact_id)
            };
            let url = format!("{}/{}", base, resource_name);
            let resp = client.get(&url)
                .bearer_auth(&token)
                .query(&[("personFields", "names,emailAddresses,phoneNumbers,organizations,birthdays,addresses,biographies")])
                .send().await
                .map_err(|e| Error::Tool(format!("Google People API error: {}", e)))?;
            parse_google_people_response(resp).await
        }
        "create" => {
            let mut person = json!({});
            if let Some(name) = params.get("name").and_then(|v| v.as_str()) {
                let parts: Vec<&str> = name.splitn(2, ' ').collect();
                person["names"] = json!([{"givenName": parts[0], "familyName": parts.get(1).unwrap_or(&"")}]);
            }
            if let Some(email) = params.get("email").and_then(|v| v.as_str()) {
                person["emailAddresses"] = json!([{"value": email}]);
            }
            if let Some(phone) = params.get("phone").and_then(|v| v.as_str()) {
                person["phoneNumbers"] = json!([{"value": phone}]);
            }
            if let Some(org) = params.get("organization").and_then(|v| v.as_str()) {
                let title = params.get("title").and_then(|v| v.as_str()).unwrap_or("");
                person["organizations"] = json!([{"name": org, "title": title}]);
            }
            let url = format!("{}/people:createContact", base);
            let resp = client.post(&url)
                .bearer_auth(&token)
                .json(&person)
                .send().await
                .map_err(|e| Error::Tool(format!("Google People API error: {}", e)))?;
            parse_google_people_response(resp).await
        }
        "delete" => {
            let contact_id = params.get("contact_id").and_then(|v| v.as_str())
                .ok_or_else(|| Error::Tool("contact_id is required for delete".into()))?;
            let resource_name = if contact_id.starts_with("people/") {
                contact_id.to_string()
            } else {
                format!("people/{}", contact_id)
            };
            let url = format!("{}/{}:deleteContact", base, resource_name);
            let resp = client.delete(&url)
                .bearer_auth(&token)
                .send().await
                .map_err(|e| Error::Tool(format!("Google People API error: {}", e)))?;
            if resp.status().is_success() {
                Ok(json!({"status": "deleted", "contact_id": contact_id}))
            } else {
                let body: Value = resp.json().await.unwrap_or(json!({}));
                Err(Error::Tool(format!("Delete failed: {:?}", body.get("error"))))
            }
        }
        "groups" => {
            let url = format!("{}/contactGroups", base);
            let resp = client.get(&url)
                .bearer_auth(&token)
                .send().await
                .map_err(|e| Error::Tool(format!("Google People API error: {}", e)))?;
            parse_google_people_response(resp).await
        }
        "update" | "export" => {
            Err(Error::Tool(format!("Google contacts '{}' action is not yet implemented. Use the Google People API directly via http_request.", action)))
        }
        _ => Err(Error::Tool(format!("Unknown action: {}", action))),
    }
}

async fn parse_google_people_response(resp: reqwest::Response) -> Result<Value> {
    let status = resp.status();
    let body: Value = resp.json().await
        .map_err(|e| Error::Tool(format!("Failed to parse Google People response: {}", e)))?;
    if !status.is_success() {
        return Err(Error::Tool(format!("Google People API error ({}): {:?}", status, body.get("error"))));
    }
    Ok(body)
}

// ─── CardDAV ────────────────────────────────────────────────────────────────

fn resolve_carddav_config(params: &Value, ctx: &ToolContext) -> Result<(String, String, String)> {
    let api_base = params.get("api_base").and_then(|v| v.as_str())
        .or_else(|| ctx.config.get_provider("carddav").and_then(|p| p.api_base.as_deref()))
        .ok_or_else(|| Error::Tool("api_base is required for carddav".into()))?
        .to_string();
    let username = params.get("username").and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let token = params.get("api_token").and_then(|v| v.as_str())
        .or_else(|| ctx.config.get_provider("carddav").map(|p| p.api_key.as_str()))
        .unwrap_or("")
        .to_string();
    Ok((api_base, username, token))
}

async fn execute_carddav(action: &str, params: &Value, ctx: &ToolContext) -> Result<Value> {
    let (api_base, username, token) = resolve_carddav_config(params, ctx)?;
    let client = Client::new();

    match action {
        "list" | "search" => {
            // PROPFIND to list contacts
            let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let body = if query.is_empty() {
                r#"<?xml version="1.0" encoding="utf-8"?>
<C:addressbook-query xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:carddav">
  <D:prop><D:getetag/><C:address-data/></D:prop>
</C:addressbook-query>"#.to_string()
            } else {
                format!(r#"<?xml version="1.0" encoding="utf-8"?>
<C:addressbook-query xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:carddav">
  <D:prop><D:getetag/><C:address-data/></D:prop>
  <C:filter>
    <C:prop-filter name="FN">
      <C:text-match collation="i;unicode-casemap" match-type="contains">{}</C:text-match>
    </C:prop-filter>
  </C:filter>
</C:addressbook-query>"#, query)
            };

            let mut req = client.request(reqwest::Method::from_bytes(b"REPORT").unwrap_or(reqwest::Method::POST), &api_base)
                .header("Content-Type", "application/xml; charset=utf-8")
                .header("Depth", "1")
                .body(body);
            if !username.is_empty() && !token.is_empty() {
                req = req.basic_auth(&username, Some(&token));
            } else if !token.is_empty() {
                req = req.bearer_auth(&token);
            }

            let resp = req.send().await
                .map_err(|e| Error::Tool(format!("CardDAV request failed: {}", e)))?;
            let status = resp.status();
            let text = resp.text().await
                .map_err(|e| Error::Tool(format!("Failed to read CardDAV response: {}", e)))?;

            if !status.is_success() && status.as_u16() != 207 {
                return Err(Error::Tool(format!("CardDAV error ({}): {}", status, &text[..text.len().min(500)])));
            }

            // Parse vCards from response (simplified)
            let vcards: Vec<Value> = parse_vcards_from_xml(&text);
            Ok(json!({"contacts": vcards, "count": vcards.len()}))
        }
        _ => Err(Error::Tool(format!("CardDAV action '{}' is not yet implemented.", action))),
    }
}

/// Simple vCard extraction from CardDAV XML response.
fn parse_vcards_from_xml(xml: &str) -> Vec<Value> {
    let mut results = Vec::new();
    // Find all vCard data between <card:address-data> or <C:address-data> tags
    let vcard_pattern = regex::Regex::new(r"(?s)BEGIN:VCARD.*?END:VCARD").ok();
    if let Some(re) = vcard_pattern {
        for cap in re.find_iter(xml) {
            let vcard = cap.as_str();
            let mut contact = json!({});
            for line in vcard.lines() {
                let line = line.trim();
                if line.starts_with("FN:") || line.starts_with("FN;") {
                    let val = line.split_once(':').map(|x| x.1).unwrap_or("");
                    contact["name"] = json!(val);
                } else if line.starts_with("EMAIL") {
                    let val = line.split_once(':').map(|x| x.1).unwrap_or("");
                    if !val.is_empty() {
                        let emails = contact.get("emails").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                        let mut emails = emails;
                        emails.push(json!(val));
                        contact["emails"] = json!(emails);
                    }
                } else if line.starts_with("TEL") {
                    let val = line.split_once(':').map(|x| x.1).unwrap_or("");
                    if !val.is_empty() {
                        let phones = contact.get("phones").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                        let mut phones = phones;
                        phones.push(json!(val));
                        contact["phones"] = json!(phones);
                    }
                } else if line.starts_with("ORG:") || line.starts_with("ORG;") {
                    let val = line.split_once(':').map(|x| x.1).unwrap_or("");
                    contact["organization"] = json!(val.replace(';', " ").trim());
                } else if line.starts_with("TITLE:") {
                    let val = line.split_once(':').map(|x| x.1).unwrap_or("");
                    contact["title"] = json!(val);
                }
            }
            if contact.get("name").is_some() {
                results.push(contact);
            }
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_tool() -> ContactsTool { ContactsTool }

    #[test]
    fn test_schema() {
        let tool = make_tool();
        let schema = tool.schema();
        assert_eq!(schema.name, "contacts");
        assert!(schema.parameters["properties"]["source"].is_object());
    }

    #[test]
    fn test_validate_valid() {
        let tool = make_tool();
        assert!(tool.validate(&json!({"source": "macos", "action": "list"})).is_ok());
        assert!(tool.validate(&json!({"source": "google", "action": "search"})).is_ok());
        assert!(tool.validate(&json!({"source": "carddav", "action": "groups"})).is_ok());
    }

    #[test]
    fn test_validate_invalid_source() {
        let tool = make_tool();
        assert!(tool.validate(&json!({"source": "outlook", "action": "list"})).is_err());
    }

    #[test]
    fn test_validate_invalid_action() {
        let tool = make_tool();
        assert!(tool.validate(&json!({"source": "macos", "action": "sync"})).is_err());
    }

    #[test]
    fn test_parse_vcards() {
        let xml = r#"
<multistatus>
  <response>
    <propstat>
      <prop>
        <address-data>BEGIN:VCARD
VERSION:3.0
FN:John Doe
EMAIL:john@example.com
TEL:+1234567890
ORG:Acme Inc
TITLE:Engineer
END:VCARD</address-data>
      </prop>
    </propstat>
  </response>
</multistatus>"#;
        let contacts = parse_vcards_from_xml(xml);
        assert_eq!(contacts.len(), 1);
        assert_eq!(contacts[0]["name"], "John Doe");
        assert_eq!(contacts[0]["organization"], "Acme Inc");
    }

    #[test]
    fn test_validate_all_actions() {
        let tool = make_tool();
        for action in &["list", "search", "get", "create", "update", "delete", "export", "groups"] {
            assert!(tool.validate(&json!({"source": "macos", "action": action})).is_ok());
        }
    }
}
