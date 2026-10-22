use async_trait::async_trait;
use blockcell_core::{Error, Result};
use serde_json::{json, Value};
use tracing::debug;

use crate::{Tool, ToolContext, ToolSchema};

/// The only cipher the `openssl enc` backend provides. AEAD modes (aes-256-gcm,
/// chacha20-poly1305) are not available through `openssl enc`, so they are rejected
/// instead of being silently substituted.
const SUPPORTED_ALGORITHM: &str = "aes-256-cbc";
/// Header written in front of raw-key ciphertext: 16-byte magic + 32 hex chars of IV.
const KEY_MODE_MAGIC: &[u8; 16] = b"BLOCKCELL-IV-V1\n";
const KEY_MODE_IV_HEX_LEN: usize = 32;

/// Key mode pipes the whole file through openssl stdin/stdout in memory, so cap
/// the input size instead of letting a multi-GB file exhaust the process.
const KEY_MODE_MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;

/// Encryption and security tool.
///
/// Actions:
/// - **encrypt_file** / **decrypt_file**: AES-256-CBC file encryption/decryption (openssl enc backend)
/// - **generate_password**: Cryptographically secure password generation
/// - **generate_key**: Generate encryption keys (AES-256, random bytes)
/// - **hash_file**: Compute file hash (SHA-256, SHA-512)
/// - **hash_text**: Compute text hash
/// - **encode** / **decode**: Base64 / hex encoding/decoding
/// - **checksum_verify**: Verify file against expected checksum
pub struct EncryptTool;

#[async_trait]
impl Tool for EncryptTool {
    fn schema(&self) -> ToolSchema {
        let mut props = serde_json::Map::new();
        props.insert("action".into(), json!({"type": "string", "description": "Action: encrypt_file|decrypt_file|generate_password|generate_key|hash_file|hash_text|encode|decode|checksum_verify"}));
        props.insert("path".into(), json!({"type": "string", "description": "(encrypt_file/decrypt_file/hash_file/checksum_verify) Input file path"}));
        props.insert("output_path".into(), json!({"type": "string", "description": "(encrypt_file/decrypt_file) Output file path. Default: input + .enc / removes .enc"}));
        props.insert("password".into(), json!({"type": "string", "description": "(encrypt_file/decrypt_file) Encryption password. Used to derive AES-256 key via PBKDF2."}));
        props.insert("key".into(), json!({"type": "string", "description": "(encrypt_file/decrypt_file) Raw AES-256 key as hex string (64 hex chars). Alternative to password."}));
        props.insert("algorithm".into(), json!({"type": "string", "enum": ["aes-256-cbc"], "description": "(encrypt_file/decrypt_file) Encryption algorithm. Only aes-256-cbc is available (openssl enc backend). Default: aes-256-cbc"}));
        props.insert("length".into(), json!({"type": "integer", "description": "(generate_password) Password length. Default: 20. (generate_key) Key size in bits: 128/256. Default: 256"}));
        props.insert("charset".into(), json!({"type": "string", "enum": ["alphanumeric", "ascii", "numeric", "hex", "custom"], "description": "(generate_password) Character set. Default: ascii (letters+digits+symbols)"}));
        props.insert("custom_chars".into(), json!({"type": "string", "description": "(generate_password) Custom character set when charset='custom'"}));
        props.insert(
            "exclude_chars".into(),
            json!({"type": "string", "description": "(generate_password) Characters to exclude"}),
        );
        props.insert("count".into(), json!({"type": "integer", "description": "(generate_password) Number of passwords to generate. Default: 1. Max: 100"}));
        props.insert("hash_algorithm".into(), json!({"type": "string", "enum": ["sha256", "sha512"], "description": "(hash_file/hash_text/checksum_verify) Hash algorithm. Default: sha256"}));
        props.insert(
            "text".into(),
            json!({"type": "string", "description": "(hash_text/encode/decode) Input text"}),
        );
        props.insert("encoding".into(), json!({"type": "string", "enum": ["base64", "hex", "url"], "description": "(encode/decode) Encoding format. Default: base64"}));
        props.insert("expected_hash".into(), json!({"type": "string", "description": "(checksum_verify) Expected hash value to verify against"}));

        ToolSchema {
            name: "encrypt".to_string(),
            description: "Encryption, hashing, and encoding utilities. You MUST provide `action`. action='encrypt_file'|'decrypt_file': requires `path` and either `password` or `key`, optional `output_path`. action='generate_password': optional `length`, `charset`, `exclude_chars`. action='generate_key': optional `length` (key size in bits: 128/256). action='hash_file': requires `path`, optional `hash_algorithm`. action='hash_text': requires `text`, optional `hash_algorithm`. action='encode'|'decode': requires `text`, optional `encoding`. action='checksum_verify': requires `path` and `expected_hash`, optional `hash_algorithm`.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": Value::Object(props),
                "required": ["action"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let valid = [
            "encrypt_file",
            "decrypt_file",
            "generate_password",
            "generate_key",
            "hash_file",
            "hash_text",
            "encode",
            "decode",
            "checksum_verify",
        ];
        if !valid.contains(&action) {
            return Err(Error::Tool(format!(
                "Invalid action '{}'. Valid: {}",
                action,
                valid.join(", ")
            )));
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let action = params["action"].as_str().unwrap_or("");
        debug!(action = action, "encrypt execute");

        match action {
            "encrypt_file" => action_encrypt_file(&params, &ctx).await,
            "decrypt_file" => action_decrypt_file(&params, &ctx).await,
            "generate_password" => action_generate_password(&params),
            "generate_key" => action_generate_key(&params),
            "hash_file" => action_hash_file(&params).await,
            "hash_text" => action_hash_text(&params),
            "encode" => action_encode(&params),
            "decode" => action_decode(&params),
            "checksum_verify" => action_checksum_verify(&params).await,
            _ => Err(Error::Tool(format!("Unknown action: {}", action))),
        }
    }
}

// ─── File encryption (AES-256-CBC via openssl CLI) ──────────────────────────

async fn action_encrypt_file(params: &Value, ctx: &ToolContext) -> Result<Value> {
    let path = params
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Tool("path is required for encrypt_file".into()))?;
    let password = params.get("password").and_then(|v| v.as_str());
    let key_hex = params.get("key").and_then(|v| v.as_str());

    if password.is_none() && key_hex.is_none() {
        return Err(Error::Tool(
            "Either 'password' or 'key' is required for encrypt_file".into(),
        ));
    }

    let output_path = params
        .get("output_path")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("{}.enc", path));

    let resolved_path = resolve_path(path, ctx);
    let resolved_output = resolve_path(&output_path, ctx);

    if !std::path::Path::new(&resolved_path).exists() {
        return Err(Error::Tool(format!("File not found: {}", resolved_path)));
    }

    // Use openssl enc for AES-256-CBC encryption
    let algorithm = params
        .get("algorithm")
        .and_then(|v| v.as_str())
        .unwrap_or(SUPPORTED_ALGORITHM);
    if algorithm != SUPPORTED_ALGORITHM {
        return Err(Error::Validation(format!(
            "unsupported algorithm '{algorithm}': the openssl enc backend only provides {SUPPORTED_ALGORITHM}"
        )));
    }

    if let Some(pw) = password {
        // The passphrase travels over stdin (`-pass stdin`), never argv: a
        // `-pass pass:…` argument is visible to every other process via `ps`.
        let args = vec![
            "enc".to_string(),
            "-aes-256-cbc".to_string(),
            "-salt".to_string(),
            "-pbkdf2".to_string(),
            "-iter".to_string(),
            "100000".to_string(),
            "-in".to_string(),
            resolved_path.clone(),
            "-out".to_string(),
            resolved_output.clone(),
            "-pass".to_string(),
            "stdin".to_string(),
        ];
        run_openssl_enc(&args, Some(passphrase_stdin(pw))).await?;
    } else if let Some(k) = key_hex {
        if k.len() != 64 {
            return Err(Error::Tool(
                "key must be 64 hex characters (256 bits)".into(),
            ));
        }
        // Raw-key mode: openssl `enc` does not persist an explicit `-iv`, so the IV is
        // kept in a header in front of the ciphertext. Without it the output could never
        // be decrypted (the previous implementation generated an IV and dropped it).
        // The cipher itself runs in-process: passing -K/-iv through openssl argv would
        // expose the raw key in the process table.
        let file_len = std::fs::metadata(&resolved_path)
            .map(|m| m.len())
            .unwrap_or(0);
        if file_len > KEY_MODE_MAX_FILE_BYTES {
            return Err(Error::Tool(format!(
                "file too large for key-mode encryption: {} bytes exceeds {}-byte limit",
                file_len, KEY_MODE_MAX_FILE_BYTES
            )));
        }
        let path_c = resolved_path.clone();
        let plaintext = tokio::task::spawn_blocking(move || std::fs::read(path_c))
            .await
            .map_err(|e| Error::Tool(format!("Read task failed: {}", e)))?
            .map_err(|e| Error::Tool(format!("Failed to read {}: {}", resolved_path, e)))?;
        let (ciphertext, iv_hex) = aes256_cbc_encrypt(k, &plaintext)?;

        let mut payload =
            Vec::with_capacity(KEY_MODE_MAGIC.len() + iv_hex.len() + ciphertext.len());
        payload.extend_from_slice(KEY_MODE_MAGIC);
        payload.extend_from_slice(iv_hex.as_bytes());
        payload.extend_from_slice(&ciphertext);
        let out_c = resolved_output.clone();
        tokio::task::spawn_blocking(move || std::fs::write(out_c, payload))
            .await
            .map_err(|e| Error::Tool(format!("Write task failed: {}", e)))?
            .map_err(|e| Error::Tool(format!("Failed to write {}: {}", resolved_output, e)))?;
    }

    let file_size = std::fs::metadata(&resolved_output)
        .map(|m| m.len())
        .unwrap_or(0);

    Ok(json!({
        "status": "encrypted",
        "input": resolved_path,
        "output": resolved_output,
        "algorithm": "aes-256-cbc",
        "kdf": "pbkdf2",
        "output_size": file_size,
    }))
}

async fn action_decrypt_file(params: &Value, ctx: &ToolContext) -> Result<Value> {
    let path = params
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Tool("path is required for decrypt_file".into()))?;
    let password = params.get("password").and_then(|v| v.as_str());
    let key_hex = params.get("key").and_then(|v| v.as_str());

    if password.is_none() && key_hex.is_none() {
        return Err(Error::Tool(
            "Either 'password' or 'key' is required for decrypt_file".into(),
        ));
    }

    let output_path = params
        .get("output_path")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            if let Some(stripped) = path.strip_suffix(".enc") {
                stripped.to_string()
            } else {
                format!("{}.dec", path)
            }
        });

    let resolved_path = resolve_path(path, ctx);
    let resolved_output = resolve_path(&output_path, ctx);

    if !std::path::Path::new(&resolved_path).exists() {
        return Err(Error::Tool(format!("File not found: {}", resolved_path)));
    }

    let algorithm = params
        .get("algorithm")
        .and_then(|v| v.as_str())
        .unwrap_or(SUPPORTED_ALGORITHM);
    if algorithm != SUPPORTED_ALGORITHM {
        return Err(Error::Validation(format!(
            "unsupported algorithm '{algorithm}': the openssl enc backend only provides {SUPPORTED_ALGORITHM}"
        )));
    }

    if let Some(pw) = password {
        let args = vec![
            "enc".to_string(),
            "-aes-256-cbc".to_string(),
            "-d".to_string(),
            "-pbkdf2".to_string(),
            "-iter".to_string(),
            "100000".to_string(),
            "-in".to_string(),
            resolved_path.clone(),
            "-out".to_string(),
            resolved_output.clone(),
            "-pass".to_string(),
            "stdin".to_string(),
        ];
        run_openssl_enc(&args, Some(passphrase_stdin(pw)))
            .await
            .map_err(|e| Error::Tool(format!("Decryption failed (wrong password?): {e}")))?;
    } else if let Some(k) = key_hex {
        let file_len = std::fs::metadata(&resolved_path)
            .map(|m| m.len())
            .unwrap_or(0);
        if file_len > KEY_MODE_MAX_FILE_BYTES {
            return Err(Error::Tool(format!(
                "file too large for key-mode decryption: {} bytes exceeds {}-byte limit",
                file_len, KEY_MODE_MAX_FILE_BYTES
            )));
        }
        let path_c = resolved_path.clone();
        let raw = tokio::task::spawn_blocking(move || std::fs::read(path_c))
            .await
            .map_err(|e| Error::Tool(format!("Read task failed: {}", e)))?
            .map_err(|e| Error::Tool(format!("Failed to read {}: {}", resolved_path, e)))?;
        let header_len = KEY_MODE_MAGIC.len() + KEY_MODE_IV_HEX_LEN;
        if raw.len() < header_len || !raw.starts_with(KEY_MODE_MAGIC) {
            return Err(Error::Tool(
                "encrypted file has no IV header: it was produced by a version that did not \
                 persist the IV, so it cannot be decrypted — re-encrypt the source file"
                    .into(),
            ));
        }
        let iv_hex = std::str::from_utf8(&raw[KEY_MODE_MAGIC.len()..header_len])
            .map_err(|_| Error::Tool("encrypted file has a corrupt IV header".into()))?;
        // In-process AES-256-CBC: the raw key must not travel over openssl argv.
        let plaintext = aes256_cbc_decrypt(k, iv_hex, &raw[header_len..])
            .map_err(|e| Error::Tool(format!("Decryption failed: {e}")))?;
        let out_c = resolved_output.clone();
        tokio::task::spawn_blocking(move || std::fs::write(out_c, plaintext))
            .await
            .map_err(|e| Error::Tool(format!("Write task failed: {}", e)))?
            .map_err(|e| Error::Tool(format!("Failed to write {}: {}", resolved_output, e)))?;
    }

    let file_size = std::fs::metadata(&resolved_output)
        .map(|m| m.len())
        .unwrap_or(0);

    Ok(json!({
        "status": "decrypted",
        "input": resolved_path,
        "output": resolved_output,
        "output_size": file_size,
    }))
}

// ─── Password generation ────────────────────────────────────────────────────

fn action_generate_password(params: &Value) -> Result<Value> {
    let length = params.get("length").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
    let count = params.get("count").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
    let charset_name = params
        .get("charset")
        .and_then(|v| v.as_str())
        .unwrap_or("ascii");
    let exclude = params
        .get("exclude_chars")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if length < 4 {
        return Err(Error::Tool("Password length must be at least 4".into()));
    }
    if length > 1024 {
        return Err(Error::Tool("Password length must be at most 1024".into()));
    }
    if count > 100 {
        return Err(Error::Tool("Password count must be at most 100".into()));
    }

    let base_chars: Vec<char> = match charset_name {
        "alphanumeric" => "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789".chars().collect(),
        "numeric" => "0123456789".chars().collect(),
        "hex" => "0123456789abcdef".chars().collect(),
        "custom" => {
            let custom = params.get("custom_chars").and_then(|v| v.as_str()).unwrap_or("");
            if custom.is_empty() {
                return Err(Error::Tool("custom_chars is required when charset='custom'".into()));
            }
            custom.chars().collect()
        }
        _ => "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789!@#$%^&*()-_=+[]{}|;:,.<>?".chars().collect(),
    };

    let chars: Vec<char> = base_chars
        .into_iter()
        .filter(|c| !exclude.contains(*c))
        .collect();

    if chars.is_empty() {
        return Err(Error::Tool("No characters left after exclusion".into()));
    }

    let passwords: Vec<String> = (0..count)
        .map(|_| generate_random_string(&chars, length))
        .collect();

    if count == 1 {
        // Analyze password strength
        let pw = &passwords[0];
        let strength = analyze_password_strength(pw);
        Ok(json!({
            "password": pw,
            "length": length,
            "charset": charset_name,
            "strength": strength,
        }))
    } else {
        Ok(json!({
            "passwords": passwords,
            "count": count,
            "length": length,
            "charset": charset_name,
        }))
    }
}

fn generate_random_string(chars: &[char], length: usize) -> String {
    // 使用 OsRng 生成密码学安全的随机数
    use rand::RngCore;
    let mut rng = rand::rngs::OsRng;
    let mut result = String::with_capacity(length);
    for _ in 0..length {
        let idx = (rng.next_u64() as usize) % chars.len();
        result.push(chars[idx]);
    }
    result
}

fn analyze_password_strength(password: &str) -> Value {
    let len = password.len();
    let has_lower = password.chars().any(|c| c.is_ascii_lowercase());
    let has_upper = password.chars().any(|c| c.is_ascii_uppercase());
    let has_digit = password.chars().any(|c| c.is_ascii_digit());
    let has_special = password.chars().any(|c| !c.is_alphanumeric());
    let unique_chars = {
        let mut chars: Vec<char> = password.chars().collect();
        chars.sort();
        chars.dedup();
        chars.len()
    };

    let mut score = 0;
    if len >= 8 {
        score += 1;
    }
    if len >= 12 {
        score += 1;
    }
    if len >= 16 {
        score += 1;
    }
    if has_lower {
        score += 1;
    }
    if has_upper {
        score += 1;
    }
    if has_digit {
        score += 1;
    }
    if has_special {
        score += 1;
    }
    if unique_chars > len / 2 {
        score += 1;
    }

    let level = match score {
        0..=2 => "weak",
        3..=5 => "moderate",
        6..=7 => "strong",
        _ => "very_strong",
    };

    // Estimate entropy
    let charset_size = (if has_lower { 26 } else { 0 })
        + (if has_upper { 26 } else { 0 })
        + (if has_digit { 10 } else { 0 })
        + (if has_special { 32 } else { 0 });
    let entropy_bits = if charset_size > 0 {
        (len as f64) * (charset_size as f64).log2()
    } else {
        0.0
    };

    json!({
        "level": level,
        "score": format!("{}/8", score),
        "entropy_bits": format!("{:.1}", entropy_bits),
        "has_lowercase": has_lower,
        "has_uppercase": has_upper,
        "has_digits": has_digit,
        "has_special": has_special,
        "unique_chars": unique_chars,
    })
}

// ─── Key generation ─────────────────────────────────────────────────────────

fn action_generate_key(params: &Value) -> Result<Value> {
    let bits = params.get("length").and_then(|v| v.as_u64()).unwrap_or(256);
    let bytes = match bits {
        128 => 16,
        256 => 32,
        _ => return Err(Error::Tool("Key length must be 128 or 256 bits".into())),
    };

    let key_hex = generate_hex_bytes(bytes);

    Ok(json!({
        "key": key_hex,
        "bits": bits,
        "bytes": bytes,
        "format": "hex",
        "note": "Store this key securely. It cannot be recovered if lost.",
    }))
}

/// AES-256-CBC encrypt with a caller-supplied raw key (openssl `enc` compatible
/// PKCS#7 padding). Returns (ciphertext, hex-encoded IV).
fn aes256_cbc_encrypt(key_hex: &str, plaintext: &[u8]) -> Result<(Vec<u8>, String)> {
    use aes::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};
    type Aes256CbcEnc = cbc::Encryptor<aes::Aes256>;

    let key = hex::decode(key_hex).map_err(|_| Error::Tool("key must be valid hex".to_string()))?;
    if key.len() != 32 {
        return Err(Error::Tool("key must decode to 32 bytes".to_string()));
    }
    let mut iv = [0u8; 16];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut iv);
    let ciphertext = Aes256CbcEnc::new(key.as_slice().into(), &iv.into())
        .encrypt_padded_vec_mut::<Pkcs7>(plaintext);
    Ok((ciphertext, hex::encode(iv)))
}

/// AES-256-CBC decrypt with a caller-supplied raw key and hex IV.
fn aes256_cbc_decrypt(key_hex: &str, iv_hex: &str, ciphertext: &[u8]) -> Result<Vec<u8>> {
    use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
    type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;

    let key = hex::decode(key_hex).map_err(|_| Error::Tool("key must be valid hex".to_string()))?;
    let iv = hex::decode(iv_hex)
        .map_err(|_| Error::Tool("encrypted file has a corrupt IV header".to_string()))?;
    if iv.len() != 16 {
        return Err(Error::Tool(
            "encrypted file has a corrupt IV header".to_string(),
        ));
    }
    let mut iv_arr = [0u8; 16];
    iv_arr.copy_from_slice(&iv);
    Aes256CbcDec::new(key.as_slice().into(), &iv_arr.into())
        .decrypt_padded_vec_mut::<Pkcs7>(ciphertext)
        .map_err(|_| Error::Tool("bad key, IV, or corrupted ciphertext".to_string()))
}

/// Passphrase bytes for openssl's `-pass stdin` (a single line).
fn passphrase_stdin(password: &str) -> Vec<u8> {
    let mut bytes = password.as_bytes().to_vec();
    bytes.push(b'\n');
    bytes
}

/// Run `openssl enc` with `args`, feeding `stdin_payload` (if any) and returning stdout.
///
/// Both secrets and file payloads travel over stdin, so neither the passphrase nor the
/// plaintext/ciphertext ever appears in the process table.
async fn run_openssl_enc(args: &[String], stdin_payload: Option<Vec<u8>>) -> Result<Vec<u8>> {
    use tokio::io::AsyncWriteExt;

    let mut command = tokio::process::Command::new("openssl");
    command.args(args);
    command.stdin(std::process::Stdio::piped());
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());

    let mut child = command
        .spawn()
        .map_err(|e| Error::Tool(format!("openssl not found or failed: {}", e)))?;

    // Write stdin concurrently with output collection: a sequential write
    // deadlocks once the payload exceeds the pipe buffer (~64KB), because
    // openssl blocks writing stdout while we block writing stdin.
    let stdin_writer = stdin_payload.and_then(|payload| {
        child.stdin.take().map(|mut stdin| {
            tokio::spawn(async move {
                // `stdin` drops with the task, closing the pipe so openssl sees EOF.
                stdin.write_all(&payload).await
            })
        })
    });

    let output = child
        .wait_with_output()
        .await
        .map_err(|e| Error::Tool(format!("openssl failed: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Tool(format!("openssl failed: {}", stderr)));
    }

    if let Some(writer) = stdin_writer {
        writer
            .await
            .map_err(|e| Error::Tool(format!("openssl stdin writer failed: {}", e)))?
            .map_err(|e| Error::Tool(format!("failed to write to openssl stdin: {}", e)))?;
    }

    Ok(output.stdout)
}

fn generate_hex_bytes(count: usize) -> String {
    // 使用 OsRng 生成密码学安全的随机字节
    use rand::RngCore;
    use std::fmt::Write;
    let mut rng = rand::rngs::OsRng;
    let mut bytes = vec![0u8; count];
    rng.fill_bytes(&mut bytes);
    let mut hex = String::with_capacity(count * 2);
    for byte in bytes {
        write!(hex, "{:02x}", byte).unwrap();
    }
    hex
}

// ─── Hashing ────────────────────────────────────────────────────────────────

async fn action_hash_file(params: &Value) -> Result<Value> {
    let path = params
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Tool("path is required for hash_file".into()))?;
    let algo = params
        .get("hash_algorithm")
        .and_then(|v| v.as_str())
        .unwrap_or("sha256");

    let path = path.to_string();
    let algo = algo.to_string();

    // 克隆一份用于闭包内移动，保留原值用于闭包后的 json! 宏
    let path_for_block = path.clone();
    let algo_for_block = algo.clone();

    // 使用 spawn_blocking 在后台线程分块读取文件并计算哈希
    // 用 BufReader 分块处理，避免大文件一次性读入内存造成 OOM
    let (hash, file_size) = tokio::task::spawn_blocking(move || {
        let file =
            std::fs::File::open(&path_for_block).map_err(|e| format!("打开文件失败: {}", e))?;
        let file_size = file
            .metadata()
            .map_err(|e| format!("获取文件大小失败: {}", e))?
            .len();
        let mut reader = std::io::BufReader::with_capacity(64 * 1024, file); // 64KB 缓冲区

        let hash = match algo_for_block.as_str() {
            "sha256" => {
                use sha2::Digest;
                let mut hasher = sha2::Sha256::new();
                std::io::copy(&mut reader, &mut hasher)
                    .map_err(|e| format!("读取文件失败: {}", e))?;
                format!("{:x}", hasher.finalize())
            }
            "sha512" => {
                use sha2::Digest;
                let mut hasher = sha2::Sha512::new();
                std::io::copy(&mut reader, &mut hasher)
                    .map_err(|e| format!("读取文件失败: {}", e))?;
                format!("{:x}", hasher.finalize())
            }
            "sha1" => {
                // SHA-1 已不再安全，明确拒绝，避免静默替换导致校验失败
                return Err("SHA-1 算法已不再安全且不被支持。请使用 sha256 或 sha512".to_string());
            }
            "md5" => {
                // MD5 已不再安全，明确拒绝，避免静默替换导致校验失败
                return Err("MD5 算法已不再安全且不被支持。请使用 sha256 或 sha512".to_string());
            }
            _ => return Err(format!("未知哈希算法: {}", algo_for_block)),
        };

        Ok::<_, String>((hash, file_size))
    })
    .await
    .map_err(|e| Error::Tool(format!("后台任务出错: {}", e)))?
    .map_err(Error::Tool)?;

    Ok(json!({
        "hash": hash,
        "algorithm": algo,
        "file": path,
        "file_size": file_size,
    }))
}

fn action_hash_text(params: &Value) -> Result<Value> {
    let text = params
        .get("text")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Tool("text is required for hash_text".into()))?;
    let algo = params
        .get("hash_algorithm")
        .and_then(|v| v.as_str())
        .unwrap_or("sha256");

    use sha2::Digest;
    let hash = match algo {
        "sha256" => {
            let mut hasher = sha2::Sha256::new();
            hasher.update(text.as_bytes());
            format!("{:x}", hasher.finalize())
        }
        "sha512" => {
            let mut hasher = sha2::Sha512::new();
            hasher.update(text.as_bytes());
            format!("{:x}", hasher.finalize())
        }
        "sha1" => {
            // SHA-1 已不再安全，明确拒绝，避免静默替换导致校验失败
            return Err(Error::Tool(
                "SHA-1 算法已不再安全且不被支持。请使用 sha256 或 sha512".into(),
            ));
        }
        "md5" => {
            // MD5 已不再安全，明确拒绝，避免静默替换导致校验失败
            return Err(Error::Tool(
                "MD5 算法已不再安全且不被支持。请使用 sha256 或 sha512".into(),
            ));
        }
        _ => return Err(Error::Tool(format!("Unknown hash algorithm: {}", algo))),
    };

    Ok(json!({
        "hash": hash,
        "algorithm": algo,
        "input_length": text.len(),
    }))
}

// ─── Encoding/Decoding ─────────────────────────────────────────────────────

fn action_encode(params: &Value) -> Result<Value> {
    let text = params
        .get("text")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Tool("text is required for encode".into()))?;
    let encoding = params
        .get("encoding")
        .and_then(|v| v.as_str())
        .unwrap_or("base64");

    let encoded = match encoding {
        "base64" => {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD.encode(text.as_bytes())
        }
        "hex" => text
            .as_bytes()
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<String>(),
        "url" => urlencoding::encode(text).to_string(),
        _ => return Err(Error::Tool(format!("Unknown encoding: {}", encoding))),
    };

    Ok(json!({
        "encoded": encoded,
        "encoding": encoding,
        "input_length": text.len(),
        "output_length": encoded.len(),
    }))
}

fn action_decode(params: &Value) -> Result<Value> {
    let text = params
        .get("text")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Tool("text is required for decode".into()))?;
    let encoding = params
        .get("encoding")
        .and_then(|v| v.as_str())
        .unwrap_or("base64");

    let decoded = match encoding {
        "base64" => {
            use base64::Engine;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(text)
                .map_err(|e| Error::Tool(format!("Base64 decode error: {}", e)))?;
            String::from_utf8(bytes)
                .map_err(|e| Error::Tool(format!("UTF-8 decode error: {}", e)))?
        }
        "hex" => {
            // Reject up-front: odd length or non-ASCII would panic on
            // out-of-range / non-boundary slicing.
            if !text.is_ascii() || text.len() % 2 != 0 {
                return Err(Error::Tool("Hex input must be even-length ASCII".into()));
            }
            let bytes: std::result::Result<Vec<u8>, _> = (0..text.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&text[i..i + 2], 16))
                .collect();
            let bytes = bytes.map_err(|e| Error::Tool(format!("Hex decode error: {}", e)))?;
            String::from_utf8(bytes)
                .map_err(|e| Error::Tool(format!("UTF-8 decode error: {}", e)))?
        }
        "url" => urlencoding::decode(text)
            .map_err(|e| Error::Tool(format!("URL decode error: {}", e)))?
            .to_string(),
        _ => return Err(Error::Tool(format!("Unknown encoding: {}", encoding))),
    };

    Ok(json!({
        "decoded": decoded,
        "encoding": encoding,
        "input_length": text.len(),
        "output_length": decoded.len(),
    }))
}

// ─── Checksum verification ──────────────────────────────────────────────────

async fn action_checksum_verify(params: &Value) -> Result<Value> {
    let expected = params
        .get("expected_hash")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Tool("expected_hash is required for checksum_verify".into()))?;

    let hash_result = action_hash_file(params).await?;
    let actual = hash_result
        .get("hash")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let algo = hash_result
        .get("algorithm")
        .and_then(|v| v.as_str())
        .unwrap_or("sha256");

    let matches = actual.eq_ignore_ascii_case(expected);

    Ok(json!({
        "matches": matches,
        "expected": expected,
        "actual": actual,
        "algorithm": algo,
        "file": hash_result.get("file"),
    }))
}

// ─── Helpers ────────────────────────────────────────────────────────────────

fn resolve_path(path: &str, ctx: &ToolContext) -> String {
    if path.starts_with("~/") {
        dirs::home_dir()
            .map(|h| h.join(&path[2..]).to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string())
    } else if path.starts_with('/') {
        path.to_string()
    } else {
        ctx.workspace.join(path).to_string_lossy().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_tool() -> EncryptTool {
        EncryptTool
    }

    #[test]
    fn test_schema() {
        let tool = make_tool();
        let schema = tool.schema();
        assert_eq!(schema.name, "encrypt");
        assert!(schema.parameters["properties"]["action"].is_object());
    }

    #[test]
    fn test_validate_valid() {
        let tool = make_tool();
        assert!(tool.validate(&json!({"action": "encrypt_file"})).is_ok());
        assert!(tool
            .validate(&json!({"action": "generate_password"}))
            .is_ok());
        assert!(tool.validate(&json!({"action": "hash_text"})).is_ok());
        assert!(tool.validate(&json!({"action": "encode"})).is_ok());
    }

    #[test]
    fn test_validate_invalid() {
        let tool = make_tool();
        assert!(tool.validate(&json!({"action": "crack"})).is_err());
    }

    #[test]
    fn test_generate_password() {
        let result =
            action_generate_password(&json!({"action": "generate_password", "length": 16}))
                .unwrap();
        let pw = result["password"].as_str().unwrap();
        assert_eq!(pw.len(), 16);
    }

    #[test]
    fn test_generate_password_alphanumeric() {
        let result = action_generate_password(
            &json!({"action": "generate_password", "length": 20, "charset": "alphanumeric"}),
        )
        .unwrap();
        let pw = result["password"].as_str().unwrap();
        assert_eq!(pw.len(), 20);
        assert!(pw.chars().all(|c| c.is_alphanumeric()));
    }

    #[test]
    fn test_generate_password_count_capped() {
        // Unbounded `count` used to allow count=1e9 → hang/OOM.
        assert!(
            action_generate_password(&json!({"action": "generate_password", "count": 1000}))
                .is_err()
        );
        assert!(
            action_generate_password(&json!({"action": "generate_password", "count": 100})).is_ok()
        );
    }

    #[test]
    fn test_generate_key() {
        let result = action_generate_key(&json!({"length": 256})).unwrap();
        let key = result["key"].as_str().unwrap();
        assert_eq!(key.len(), 64); // 32 bytes = 64 hex chars
    }

    #[test]
    fn test_hash_text() {
        let result =
            action_hash_text(&json!({"text": "hello", "hash_algorithm": "sha256"})).unwrap();
        let hash = result["hash"].as_str().unwrap();
        assert_eq!(
            hash,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn test_encode_decode_base64() {
        let encoded = action_encode(&json!({"text": "Hello World", "encoding": "base64"})).unwrap();
        assert_eq!(encoded["encoded"].as_str().unwrap(), "SGVsbG8gV29ybGQ=");

        let decoded =
            action_decode(&json!({"text": "SGVsbG8gV29ybGQ=", "encoding": "base64"})).unwrap();
        assert_eq!(decoded["decoded"].as_str().unwrap(), "Hello World");
    }

    #[test]
    fn test_encode_decode_hex() {
        let encoded = action_encode(&json!({"text": "Hi", "encoding": "hex"})).unwrap();
        assert_eq!(encoded["encoded"].as_str().unwrap(), "4869");

        let decoded = action_decode(&json!({"text": "4869", "encoding": "hex"})).unwrap();
        assert_eq!(decoded["decoded"].as_str().unwrap(), "Hi");
    }

    #[test]
    fn test_decode_hex_rejects_odd_length_and_non_ascii() {
        // Odd-length or non-ASCII input previously panicked on out-of-range /
        // non-boundary slicing.
        assert!(action_decode(&json!({"text": "abc", "encoding": "hex"})).is_err());
        assert!(action_decode(&json!({"text": "a中4", "encoding": "hex"})).is_err());
    }

    #[test]
    fn test_password_strength() {
        let strength = analyze_password_strength("aB3$xYz!9Qw@pL5#mN7&");
        assert_eq!(strength["level"].as_str().unwrap(), "very_strong");
        // Shorter password should be "strong"
        let strength2 = analyze_password_strength("aB3$xYz!9Qw@");
        assert_eq!(strength2["level"].as_str().unwrap(), "strong");
    }

    #[test]
    fn test_validate_all_actions() {
        let tool = make_tool();
        for action in &[
            "encrypt_file",
            "decrypt_file",
            "generate_password",
            "generate_key",
            "hash_file",
            "hash_text",
            "encode",
            "decode",
            "checksum_verify",
        ] {
            assert!(tool.validate(&json!({"action": action})).is_ok());
        }
    }

    fn test_ctx(workspace: &std::path::Path) -> ToolContext {
        ToolContext {
            workspace: workspace.to_path_buf(),
            base: workspace.to_path_buf(),
            builtin_skills_dir: None,
            active_skill_dir: None,
            session_key: "cli:test".to_string(),
            channel: "cli".to_string(),
            account_id: None,
            sender_id: Some("user".to_string()),
            sender_role: None,
            chat_id: "test".to_string(),
            config: blockcell_core::Config::default(),
            permissions: blockcell_core::types::PermissionSet::new(),
            task_manager: None,
            memory_store: None,
            memory_file_store: None,
            ghost_memory_lifecycle: None,
            skill_file_store: None,
            session_search: None,
            outbound_tx: None,
            spawn_handle: None,
            capability_registry: None,
            core_evolution: None,
            event_emitter: None,
            channel_contacts_file: None,
            response_cache: None,
            runtime_handle: None,
            agent_identity: None,
            skill_mutex: None,
            agent_type_registry: None,
            evolution_workflow_store: None,
        }
    }

    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("blockcell-encrypt-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    /// Raw-key mode must persist the IV, otherwise the ciphertext is unrecoverable.
    #[tokio::test]
    async fn key_mode_round_trip_recovers_plaintext() {
        let dir = scratch_dir("key");
        let ctx = test_ctx(&dir);
        std::fs::write(dir.join("plain.txt"), b"secret payload").expect("write source");

        let key = generate_hex_bytes(32);
        let encrypted = action_encrypt_file(
            &json!({"action": "encrypt_file", "path": "plain.txt", "key": key}),
            &ctx,
        )
        .await
        .expect("encrypt");
        let enc_path = encrypted["output"]
            .as_str()
            .expect("output path")
            .to_string();

        let raw = std::fs::read(&enc_path).expect("read ciphertext");
        assert!(
            raw.starts_with(KEY_MODE_MAGIC),
            "ciphertext must carry the IV header"
        );

        let out_path = dir.join("out.txt");
        let decrypted = action_decrypt_file(
            &json!({
                "action": "decrypt_file",
                "path": enc_path,
                "key": key,
                "output_path": out_path.to_string_lossy(),
            }),
            &ctx,
        )
        .await
        .expect("decrypt");
        assert_eq!(decrypted["status"], "decrypted");
        assert_eq!(
            std::fs::read(&out_path).expect("read plaintext"),
            b"secret payload"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The passphrase travels over stdin (`-pass stdin`), and must still round-trip.
    #[tokio::test]
    async fn password_mode_round_trip_recovers_plaintext() {
        let dir = scratch_dir("pw");
        let ctx = test_ctx(&dir);
        std::fs::write(dir.join("plain.txt"), b"passphrase payload").expect("write source");

        let encrypted = action_encrypt_file(
            &json!({"action": "encrypt_file", "path": "plain.txt", "password": "correct horse"}),
            &ctx,
        )
        .await
        .expect("encrypt");
        let enc_path = encrypted["output"]
            .as_str()
            .expect("output path")
            .to_string();

        let out_path = dir.join("out.txt");
        action_decrypt_file(
            &json!({
                "action": "decrypt_file",
                "path": enc_path,
                "password": "correct horse",
                "output_path": out_path.to_string_lossy(),
            }),
            &ctx,
        )
        .await
        .expect("decrypt");
        assert_eq!(
            std::fs::read(&out_path).expect("read plaintext"),
            b"passphrase payload"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// AEAD modes are advertised nowhere and must be rejected, not silently downgraded.
    #[tokio::test]
    async fn unsupported_algorithm_is_rejected() {
        let dir = scratch_dir("algo");
        let ctx = test_ctx(&dir);
        std::fs::write(dir.join("a.txt"), b"x").expect("write source");

        let error = action_encrypt_file(
            &json!({
                "action": "encrypt_file",
                "path": "a.txt",
                "password": "pw",
                "algorithm": "aes-256-gcm",
            }),
            &ctx,
        )
        .await
        .expect_err("aes-256-gcm must be rejected");
        assert!(format!("{error}").contains("aes-256-gcm"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
