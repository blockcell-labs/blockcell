use sha2::{Digest, Sha256};

/// 计算 session_key 的稳定哈希后缀（64 位，16 位十六进制字符）。
///
/// 使用 SHA-256 取前 8 字节，保证跨平台、跨 Rust 版本的一致性。
/// 用于文件系统路径生成，避免 [`std::collections::hash_map::DefaultHasher`]
/// 在不同 Rust 版本间不兼容的问题。
///
/// ## 设计决策
/// - 使用 SHA-256 而非 DefaultHasher：保证输出是稳定持久化格式契约
/// - 截取 64 bit（而非 32 bit）：session 数量较大时降低碰撞风险
/// - 所有写入侧和读取侧（如 `session_recall`）必须使用同一实现
pub fn stable_hash_session_key(session_key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(session_key.as_bytes());
    let result = hasher.finalize();
    // 取前 8 字节（64 位）作为十六进制后缀
    // SHA-256 输出固定 32 字节，直接索引构造 [u8; 8] 安全无 panic
    let hash_u64 = u64::from_be_bytes([
        result[0], result[1], result[2], result[3], result[4], result[5], result[6], result[7],
    ]);
    format!("{:016x}", hash_u64)
}

pub fn build_session_key(channel: &str, chat_id: &str) -> String {
    format!("{}:{}", channel, chat_id)
}

/// 将 session_key 转换为无碰撞、可逆且文件系统安全的文件名（stem）。
pub fn session_file_stem(session_key: &str) -> String {
    if session_key
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '.' | ':'))
        && !session_key.ends_with('.')
    {
        return legacy_session_file_stem(session_key);
    }

    let mut stem = String::from("s~");
    for byte in session_key.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(*byte, b'_' | b'-' | b'.') {
            stem.push(*byte as char);
        } else {
            stem.push('~');
            stem.push_str(&format!("{byte:02X}"));
        }
    }
    stem
}

/// 旧版本使用的有碰撞文件名格式。仅供显式迁移识别，不能据此猜测读取或删除归属。
pub fn legacy_session_file_stem(session_key: &str) -> String {
    session_key.replace([':', '/', '\\'], "_")
}

fn decode_session_file_stem(file_stem: &str) -> Option<String> {
    let encoded = file_stem.strip_prefix("s~")?;
    let bytes = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'~' {
            let hex = std::str::from_utf8(bytes.get(index + 1..index + 3)?).ok()?;
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

/// Return a versioned opaque API handle containing the entire canonical stem.
/// Channel and account identity are retained; clients must not decode this ID.
pub fn session_id_from_file_stem(file_stem: &str) -> String {
    format!("sid~{file_stem}")
}

fn key_from_canonical_stem(file_stem: &str) -> Option<String> {
    let key = if file_stem.starts_with("s~") {
        decode_session_file_stem(file_stem)?
    } else {
        file_stem.replace('_', ":")
    };
    let (channel, _) = key.split_once(':')?;
    if channel.is_empty() || session_file_stem(&key) != file_stem {
        return None;
    }
    Some(key)
}

fn previous_short_id<'a>(file_stem: &'a str, key: &'a str) -> Option<&'a str> {
    if file_stem.starts_with("s~") {
        key.split_once(':').map(|(_, id)| id)
    } else {
        file_stem.split_once('_').map(|(_, id)| id)
    }
}

/// Display a raw chat ID verbatim. Literal underscores are part of its identity.
pub fn session_title_from_id(session_id: &str) -> String {
    session_id.to_string()
}

/// Resolve an existing session without choosing a first matching short ID.
///
/// New clients use the opaque handle from the list or session_bound event.
/// Older raw short IDs remain accepted only if their complete candidate set is
/// unique and the spelling is lossless. The known default_<digits> automatic
/// ID is the sole lossy alias retained for compatibility; ambiguous aliases
/// always fail. Unknown handles never create or infer a new session key.
pub fn resolve_session_key_from_id<'a, I>(session_id: &str, file_stems: I) -> crate::Result<String>
where
    I: IntoIterator<Item = &'a str>,
{
    let stems: Vec<&str> = file_stems.into_iter().collect();
    if let Some(stem) = session_id.strip_prefix("sid~") {
        if !stems.contains(&stem) {
            return Err(crate::Error::NotFound("Session not found".to_string()));
        }
        return key_from_canonical_stem(stem).ok_or_else(|| {
            crate::Error::Validation("Invalid or noncanonical session handle".to_string())
        });
    }

    let mut candidates = std::collections::BTreeSet::new();
    for stem in stems {
        let Some(key) = key_from_canonical_stem(stem) else {
            continue;
        };
        let raw_id = key.split_once(':').map(|(_, id)| id);
        if raw_id == Some(session_id) || previous_short_id(stem, &key) == Some(session_id) {
            candidates.insert(key);
        }
    }
    if candidates.len() > 1 {
        return Err(crate::Error::Validation(
            "Ambiguous legacy session ID; select the opaque ID from the session list".to_string(),
        ));
    }
    let Some(key) = candidates.into_iter().next() else {
        return Err(crate::Error::NotFound("Session not found".to_string()));
    };
    let default_alias = session_id.strip_prefix("default_").is_some_and(|suffix| {
        !suffix.is_empty()
            && suffix.bytes().all(|byte| byte.is_ascii_digit())
            && key == format!("ws:default:{suffix}")
    });
    if key.split_once(':').map(|(_, id)| id) == Some(session_id) || default_alias {
        Ok(key)
    } else {
        Err(crate::Error::Validation(
            "Lossy legacy session ID requires the opaque ID from the session list".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_session_ids_preserve_channel_and_literal_separators() {
        let keys = ["ws:a:b", "ws:a_b", "telegram:a:b", "ws:a/b"];
        let ids: std::collections::HashSet<_> = keys
            .iter()
            .map(|key| session_id_from_file_stem(&session_file_stem(key)))
            .collect();
        assert_eq!(ids.len(), keys.len());
    }

    #[test]
    fn test_build_session_key() {
        assert_eq!(build_session_key("ws", "default:123"), "ws:default:123");
    }

    #[test]
    fn test_session_file_stem() {
        let colon = session_file_stem("ws:a:b");
        let underscore = session_file_stem("ws:a_b");
        let slash = session_file_stem("ws:a/b");
        let backslash = session_file_stem("ws:a\\b");

        assert_ne!(colon, underscore);
        assert_ne!(underscore, slash);
        assert_ne!(slash, backslash);
        for stem in [colon, underscore, slash, backslash] {
            assert!(!stem.contains([':', '/', '\\']));
        }
    }

    #[test]
    fn test_session_id_from_file_stem() {
        assert_eq!(
            session_id_from_file_stem(&session_file_stem("ws:default_123")),
            "sid~s~ws~3Adefault_123"
        );
        assert_eq!(
            session_id_from_file_stem("ws_default_123"),
            "sid~ws_default_123"
        );
        assert_eq!(session_title_from_id("a_b"), "a_b");
    }

    #[test]
    fn test_resolve_session_key_from_id_prefers_existing_direct_stem() {
        let stems = ["ws_default_123", "telegram_chat_1"];
        assert_eq!(
            resolve_session_key_from_id("default_123", stems.iter().copied()).unwrap(),
            "ws:default:123"
        );
    }

    #[test]
    fn test_resolve_session_key_from_id_rejects_lossy_nondefault_alias() {
        let stems = ["ws_ws_default_123", "telegram_chat_1"];
        assert!(resolve_session_key_from_id("ws_default_123", stems.iter().copied()).is_err());
    }

    #[test]
    fn resolve_handles_and_legacy_ambiguity_independently_of_enumeration_order() {
        let keys = [
            "ws:a:b",
            "ws:a_b",
            "telegram:a:b",
            "telegram:account:1:Achat",
            "telegram:account:1:Bchat",
        ];
        let mut stems: Vec<_> = keys.iter().map(|key| session_file_stem(key)).collect();
        for _ in 0..2 {
            for key in keys {
                let id = session_id_from_file_stem(&session_file_stem(key));
                assert_eq!(
                    resolve_session_key_from_id(&id, stems.iter().map(String::as_str)).unwrap(),
                    key
                );
            }
            assert!(resolve_session_key_from_id("a_b", stems.iter().map(String::as_str)).is_err());
            assert!(resolve_session_key_from_id("a:b", stems.iter().map(String::as_str)).is_err());
            stems.reverse();
        }
    }

    #[test]
    fn legacy_default_alias_requires_a_unique_existing_session() {
        let a = session_file_stem("ws:default:123");
        let b = session_file_stem("ws:default_123");
        assert_eq!(
            resolve_session_key_from_id("default_123", [a.as_str()]).unwrap(),
            "ws:default:123"
        );
        assert!(resolve_session_key_from_id("default_123", [a.as_str(), b.as_str()]).is_err());
        assert!(resolve_session_key_from_id("default_123", [b.as_str(), a.as_str()]).is_err());
        assert!(resolve_session_key_from_id("missing", std::iter::empty()).is_err());
        assert!(resolve_session_key_from_id("a_b", ["ws_a_b"]).is_err());
    }

    #[test]
    fn invalid_or_noncanonical_handles_are_rejected() {
        assert!(resolve_session_key_from_id("sid~s~ws~3aa_b", ["s~ws~3aa_b"]).is_err());
        assert!(resolve_session_key_from_id("sid~../outside", ["../outside"]).is_err());
        assert!(resolve_session_key_from_id("sid~ws_missing", ["ws_other"]).is_err());
    }
}
