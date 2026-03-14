pub fn build_session_key(channel: &str, chat_id: &str) -> String {
    format!("{}:{}", channel, chat_id)
}

pub fn session_file_stem(session_key: &str) -> String {
    session_key.replace([':', '/', '\\'], "_")
}

pub fn session_id_from_file_stem(file_stem: &str) -> String {
    file_stem
        .find('_')
        .map(|pos| file_stem[pos + 1..].to_string())
        .unwrap_or_else(|| file_stem.to_string())
}

pub fn session_title_from_id(session_id: &str) -> String {
    session_id.replace('_', ":")
}

pub fn resolve_session_key_from_id<'a, I>(session_id: &str, file_stems: I) -> String
where
    I: IntoIterator<Item = &'a str>,
{
    let stems: Vec<&str> = file_stems.into_iter().collect();
    let normalized_id = session_id.replace(':', "_");
    let direct_key = build_session_key("ws", &session_title_from_id(session_id));
    let direct_stem = session_file_stem(&direct_key);

    if stems.iter().any(|stem| **stem == direct_stem) {
        return direct_key;
    }

    for file_stem in stems {
        if file_stem == normalized_id || session_id_from_file_stem(file_stem) == normalized_id {
            return file_stem.replace('_', ":");
        }
    }

    direct_key
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_session_key() {
        assert_eq!(build_session_key("ws", "default:123"), "ws:default:123");
    }

    #[test]
    fn test_session_file_stem() {
        assert_eq!(session_file_stem("ws:default:123"), "ws_default_123");
        assert_eq!(session_file_stem("cli/run\\test"), "cli_run_test");
    }

    #[test]
    fn test_session_id_from_file_stem() {
        assert_eq!(session_id_from_file_stem("ws_default_123"), "default_123");
        assert_eq!(session_id_from_file_stem("default_123"), "123");
    }

    #[test]
    fn test_resolve_session_key_from_id_prefers_existing_direct_stem() {
        let stems = vec!["ws_default_123", "telegram_chat_1"];
        assert_eq!(
            resolve_session_key_from_id("default_123", stems.iter().copied()),
            "ws:default:123"
        );
    }

    #[test]
    fn test_resolve_session_key_from_id_falls_back_to_matching_stem() {
        let stems = vec!["ws_ws_default_123", "telegram_chat_1"];
        assert_eq!(
            resolve_session_key_from_id("ws_default_123", stems.iter().copied()),
            "ws:ws:default:123"
        );
    }
}
