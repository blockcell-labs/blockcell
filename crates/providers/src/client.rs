use reqwest::{Client, Proxy};
use std::time::Duration;
use tracing::{info, warn};

/// 代理解析结果
enum ProxyResolution {
    /// 使用指定代理 URL
    UseProxy(String),
    /// 强制直连（provider 显式设置 proxy = ""）
    ForceDirectConnect,
    /// 未配置，跟随系统/环境变量
    None,
}

/// 判断目标 host 是否命中 no_proxy 规则。
/// 支持：精确匹配、IP 精确匹配、通配前缀 "*.example.com" / ".example.com"
fn is_no_proxy(host: &str, no_proxy_list: &[String]) -> bool {
    let host_lower = host.to_lowercase();
    for rule in no_proxy_list {
        let r = rule.trim().to_lowercase();
        if r.is_empty() {
            continue;
        }
        if let Some(suffix) = r.strip_prefix("*.") {
            // *.example.com — 只匹配子域名 x.example.com，不匹配裸的 example.com
            if host_lower.ends_with(&format!(".{}", suffix)) {
                return true;
            }
        } else if let Some(suffix) = r.strip_prefix('.') {
            // .example.com — 同时匹配 example.com 本身和 x.example.com
            if host_lower == suffix || host_lower.ends_with(&format!(".{}", suffix)) {
                return true;
            }
        } else if host_lower == r {
            // 精确匹配
            return true;
        }
    }
    false
}

/// 从 URL 中提取 host 部分，用于 no_proxy 匹配
fn extract_host(url: &str) -> Option<String> {
    // 简单解析：去掉协议头，取 host:port 部分
    let without_scheme = if let Some(pos) = url.find("://") {
        &url[pos + 3..]
    } else {
        url
    };
    let host = without_scheme.split('/').next()?.split('@').last()?;
    // 去掉端口
    let host = if host.starts_with('[') {
        // IPv6
        host.split(']').next().map(|s| s.trim_start_matches('['))?.to_string()
    } else {
        host.split(':').next()?.to_string()
    };
    Some(host)
}

/// 计算对指定 api_base URL 最终生效的代理设置。
///
/// 优先级：
/// 1. provider 级别 proxy（显式覆盖）
///    - `Some("http://...")` → 使用该代理
///    - `Some("")`           → 强制直连（跳过全局代理）
///    - `None`               → 进入下一级
/// 2. 全局 network.proxy（配合 no_proxy 列表过滤）
/// 3. 环境变量 HTTPS_PROXY / HTTP_PROXY（reqwest 默认行为，不做额外处理）
fn resolve_proxy(
    provider_proxy: Option<&str>,
    global_proxy: Option<&str>,
    no_proxy: &[String],
    api_base: &str,
) -> ProxyResolution {
    // 优先级1：provider 专属设置
    match provider_proxy {
        Some("") => return ProxyResolution::ForceDirectConnect,
        Some(p) if !p.is_empty() => return ProxyResolution::UseProxy(p.to_string()),
        _ => {}
    }

    // 优先级2：全局代理 + no_proxy 过滤
    if let Some(global) = global_proxy {
        if !global.is_empty() {
            // 检查 api_base host 是否在 no_proxy 列表中
            if !no_proxy.is_empty() {
                if let Some(host) = extract_host(api_base) {
                    if is_no_proxy(&host, no_proxy) {
                        return ProxyResolution::ForceDirectConnect;
                    }
                }
            }
            return ProxyResolution::UseProxy(global.to_string());
        }
    }

    // 优先级3：让 reqwest 自行读取环境变量（不做任何设置）
    ProxyResolution::None
}

/// 构建带代理配置的 reqwest::Client。
///
/// - `provider_proxy`: 来自 `ProviderConfig.proxy`（None=未配置，Some("")=强制直连，Some("http://...")=使用代理）
/// - `global_proxy`: 来自 `Config.network.proxy`
/// - `no_proxy`: 来自 `Config.network.no_proxy`
/// - `api_base`: provider 的 api_base URL，用于 no_proxy 匹配
/// - `timeout`: HTTP 超时时长
pub fn build_http_client(
    provider_proxy: Option<&str>,
    global_proxy: Option<&str>,
    no_proxy: &[String],
    api_base: &str,
    timeout: Duration,
) -> Client {
    let mut builder = Client::builder().timeout(timeout);

    match resolve_proxy(provider_proxy, global_proxy, no_proxy, api_base) {
        ProxyResolution::UseProxy(proxy_url) => {
            match Proxy::all(&proxy_url) {
                Ok(p) => {
                    info!(proxy = %proxy_url, api_base = %api_base, "LLM provider using proxy");
                    builder = builder.proxy(p);
                }
                Err(e) => {
                    warn!(error = %e, proxy = %proxy_url, "Invalid proxy URL, falling back to direct connect");
                }
            }
        }
        ProxyResolution::ForceDirectConnect => {
            // no_proxy() 禁用所有代理（包括环境变量），实现强制直连
            info!(api_base = %api_base, "LLM provider forced to direct connect (proxy disabled)");
            builder = builder.no_proxy();
        }
        ProxyResolution::None => {
            // 不做任何设置，reqwest 自动读取 HTTPS_PROXY/HTTP_PROXY 环境变量
        }
    }

    builder.build().unwrap_or_else(|e| {
        warn!(error = %e, "Failed to build HTTP client with proxy, using default");
        Client::new()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_no_proxy_exact() {
        let list = vec!["localhost".to_string(), "127.0.0.1".to_string()];
        assert!(is_no_proxy("localhost", &list));
        assert!(is_no_proxy("127.0.0.1", &list));
        assert!(!is_no_proxy("example.com", &list));
    }

    #[test]
    fn test_is_no_proxy_wildcard_star() {
        let list = vec!["*.local".to_string(), "*.internal.example.com".to_string()];
        assert!(is_no_proxy("myhost.local", &list));
        assert!(is_no_proxy("api.internal.example.com", &list));
        assert!(!is_no_proxy("example.com", &list));
        assert!(!is_no_proxy("local", &list)); // 无子域名，不匹配 *.local
    }

    #[test]
    fn test_is_no_proxy_dot_prefix() {
        let list = vec![".example.com".to_string()];
        assert!(is_no_proxy("api.example.com", &list));
        assert!(is_no_proxy("example.com", &list));
        assert!(!is_no_proxy("notexample.com", &list));
    }

    #[test]
    fn test_extract_host() {
        assert_eq!(extract_host("https://api.openai.com/v1"), Some("api.openai.com".to_string()));
        assert_eq!(extract_host("http://localhost:11434"), Some("localhost".to_string()));
        assert_eq!(extract_host("https://user:pass@proxy.local:8080/path"), Some("proxy.local".to_string()));
    }

    #[test]
    fn test_resolve_proxy_provider_wins() {
        // provider 有专属代理，应覆盖全局
        let r = resolve_proxy(
            Some("http://provider-proxy:8080"),
            Some("http://global-proxy:7890"),
            &[],
            "https://api.openai.com/v1",
        );
        matches!(r, ProxyResolution::UseProxy(url) if url == "http://provider-proxy:8080");
    }

    #[test]
    fn test_resolve_proxy_force_direct() {
        // provider.proxy = "" 强制直连，即使全局有代理
        let r = resolve_proxy(
            Some(""),
            Some("http://global-proxy:7890"),
            &[],
            "https://api.openai.com/v1",
        );
        assert!(matches!(r, ProxyResolution::ForceDirectConnect));
    }

    #[test]
    fn test_resolve_proxy_no_proxy_blocks_global() {
        // localhost 命中 no_proxy，全局代理不生效
        let no_proxy = vec!["localhost".to_string(), "*.local".to_string()];
        let r = resolve_proxy(
            None,
            Some("http://global-proxy:7890"),
            &no_proxy,
            "http://localhost:11434",
        );
        assert!(matches!(r, ProxyResolution::ForceDirectConnect));
    }

    #[test]
    fn test_resolve_proxy_global_used() {
        // 无 provider 代理，目标不在 no_proxy，使用全局代理
        let r = resolve_proxy(
            None,
            Some("http://global-proxy:7890"),
            &["localhost".to_string()],
            "https://api.anthropic.com/v1",
        );
        assert!(matches!(r, ProxyResolution::UseProxy(_)));
    }

    #[test]
    fn test_resolve_proxy_none() {
        // 没有任何代理配置
        let r = resolve_proxy(None, None, &[], "https://api.openai.com/v1");
        assert!(matches!(r, ProxyResolution::None));
    }

    #[test]
    fn test_build_http_client_no_proxy() {
        // 能正常构建出 Client
        let client = build_http_client(
            None,
            None,
            &[],
            "https://api.openai.com/v1",
            Duration::from_secs(30),
        );
        // reqwest::Client 不提供公开的 proxy 检查，只验证构建不 panic
        drop(client);
    }
}
