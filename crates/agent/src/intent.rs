use regex::Regex;
use std::collections::HashSet;

/// Intent categories for user messages.
/// Used to determine which tools, rules, and domain knowledge to load.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum IntentCategory {
    /// 日常闲聊、问候、闲谈 — 不需要任何工具
    Chat,
    /// 文件/代码操作 — read_file, write_file, edit_file, list_dir, exec, file_ops
    FileOps,
    /// 网页/搜索 — web_search, web_fetch, browse
    WebSearch,
    /// 金融/股票/加密货币 — exchange_api, alert_rule, stream_subscribe, ...
    Finance,
    /// 区块链/DeFi/NFT — blockchain_rpc, blockchain_tx, contract_security, bridge_api, nft_market, multisig
    Blockchain,
    /// 数据处理/可视化 — data_process, chart_generate, office_write
    DataAnalysis,
    /// 通信/邮件/社交 — email, social_media, notification, message
    Communication,
    /// 系统/硬件/应用控制/Android — system_info, app_control, camera_capture, termux_api
    SystemControl,
    /// 日程/任务/记忆 — calendar_api, cron, memory_*, knowledge_graph, list_tasks
    Organization,
    /// IoT/智能家居 — iot_control
    IoT,
    /// 媒体处理 — audio_transcribe, tts, ocr, image_understand, video_process
    Media,
    /// 开发/运维 — git_api, cloud_api, network_monitor, encrypt
    DevOps,
    /// 健康/生活 — health_api, map_api, contacts
    Lifestyle,
    /// 无法判断 — 加载核心工具集
    Unknown,
}

struct IntentRule {
    category: IntentCategory,
    keywords: Vec<&'static str>,
    patterns: Vec<Regex>,
    negative: Vec<&'static str>,
    priority: u8,
}

pub struct IntentClassifier {
    rules: Vec<IntentRule>,
}

impl Default for IntentClassifier {
    fn default() -> Self {
        Self::new()
    }
}

impl IntentClassifier {
    pub fn new() -> Self {
        let rules = vec![
            // ── Chat (highest priority) ──
            IntentRule {
                category: IntentCategory::Chat,
                keywords: vec![],
                patterns: vec![
                    Regex::new(r"(?i)^(你好|hi|hello|hey|嗨|早安|晚安|早上好|下午好|晚上好|good\s*(morning|afternoon|evening))[\s!！。.？?~～]*$").unwrap(),
                    Regex::new(r"(?i)^(谢谢|感谢|辛苦了|好的|明白了|知道了|ok|okay|got\s*it|thanks|thank\s*you)[\s!！。.？?~～]*$").unwrap(),
                    Regex::new(r"(?i)^(再见|拜拜|bye|goodbye|see\s*you)[\s!！。.？?~～]*$").unwrap(),
                    Regex::new(r"(?i)^(你是谁|who\s*are\s*you|你能做什么|what\s*can\s*you\s*do|帮助|help)[\s？?]*$").unwrap(),
                    Regex::new(r"(?i)^(哈哈|嘿嘿|呵呵|lol|haha|😂|👍|🙏|❤️|😊)[\s!！。.？?~～]*$").unwrap(),
                ],
                negative: vec![],
                priority: 10,
            },
            // ── Finance ──
            IntentRule {
                category: IntentCategory::Finance,
                keywords: vec![
                    "股票", "股价", "行情", "K线", "k线", "MACD", "macd", "涨跌", "涨停", "跌停",
                    "基金", "期货", "外汇", "加密货币", "比特币", "以太坊",
                    "茅台", "平安", "腾讯", "阿里", "招商银行", "宁德时代", "比亚迪",
                    "A股", "a股", "港股", "美股", "沪深", "创业板", "科创板",
                    "市值", "PE", "PB", "ROE", "分红", "股息", "财报", "年报",
                    "资金流", "北向资金", "龙虎榜", "大盘", "指数",
                    "债券", "国债", "可转债", "收益率曲线",
                    "ETF", "etf", "基金净值", "上证", "深证", "沪深300", "沪深", "恒生",
                    "stock", "crypto", "bitcoin", "ethereum", "forex", "trading",
                    "portfolio", "dividend", "earnings",
                ],
                patterns: vec![
                    Regex::new(r"(?i)(^|[^a-zA-Z])(BTC|ETH|SOL|DOGE|XRP|BNB|USDT|USDC)([^a-zA-Z]|$)").unwrap(),
                    Regex::new(r"(^|[^0-9])[036]\d{5}([^0-9]|$)").unwrap(),  // 6-digit A-share codes
                    Regex::new(r"(?i)\d{5}\.HK").unwrap(), // HK stocks
                    Regex::new(r"(?i)(^|[^a-zA-Z])(AAPL|MSFT|GOOG|GOOGL|AMZN|TSLA|NVDA|META)([^a-zA-Z]|$)").unwrap(),
                ],
                negative: vec![],
                priority: 7,
            },
            // ── Blockchain ──
            IntentRule {
                category: IntentCategory::Blockchain,
                keywords: vec![
                    "区块链", "智能合约", "合约", "DeFi", "defi", "NFT", "nft",
                    "链上", "Gas", "gas", "Gwei", "gwei", "ERC20", "erc20", "ERC721",
                    "钱包", "私钥", "签名", "交易哈希", "tx", "转账",
                    "Uniswap", "uniswap", "Aave", "aave", "OpenSea", "opensea",
                    "跨链", "bridge", "多签", "Safe", "Gnosis",
                    "代币安全", "合约审计", "rug pull", "honeypot",
                    "Solana", "solana", "Tron", "tron", "TRC20", "trc20", "SPL",
                    "blockchain", "smart contract", "token", "mint", "swap",
                    "approve", "revoke", "multicall",
                ],
                patterns: vec![
                    Regex::new(r"(?i)0x[a-f0-9]{40}").unwrap(), // ETH address
                    Regex::new(r"(?i)0x[a-f0-9]{64}").unwrap(), // TX hash
                ],
                negative: vec![],
                priority: 7,
            },
            // ── FileOps ──
            IntentRule {
                category: IntentCategory::FileOps,
                keywords: vec![
                    "文件", "读取", "写入", "创建文件", "删除文件", "目录", "文件夹",
                    "代码", "编辑", "修改文件", "运行", "执行", "编译", "脚本",
                    "压缩", "解压", "PDF", "pdf",
                ],
                patterns: vec![
                    Regex::new(r"\.(py|rs|js|ts|go|java|cpp|c|h|md|txt|json|yaml|yml|toml|csv|xlsx|sh|sql|html|css)(\s|$)").unwrap(),
                    Regex::new(r"[/\\][\w._-]+[/\\][\w._-]+").unwrap(), // path pattern
                    Regex::new(r"(?i)\b(cat|ls|mkdir|rm|cp|mv|grep|find|chmod)\b").unwrap(),
                ],
                negative: vec![],
                priority: 5,
            },
            // ── WebSearch ──
            IntentRule {
                category: IntentCategory::WebSearch,
                keywords: vec![
                    "搜索", "搜一下", "查一下", "上网", "网页", "网站", "链接", "URL", "url",
                    "浏览器", "打开网页", "爬取", "抓取",
                    "search", "google", "browse", "website", "fetch",
                ],
                patterns: vec![
                    Regex::new(r"https?://").unwrap(),
                    Regex::new(r"(?i)\b(www\.)\S+").unwrap(),
                ],
                negative: vec![],
                priority: 5,
            },
            // ── DataAnalysis ──
            IntentRule {
                category: IntentCategory::DataAnalysis,
                keywords: vec![
                    "数据分析", "数据处理", "统计", "图表", "可视化", "柱状图", "折线图", "饼图",
                    "CSV", "csv", "Excel", "excel", "表格",
                    "PPT", "ppt", "Word", "word", "文档", "报告",
                    "chart", "plot", "graph", "histogram", "visualization",
                    "data analysis", "spreadsheet",
                ],
                patterns: vec![],
                negative: vec![],
                priority: 5,
            },
            // ── Communication ──
            IntentRule {
                category: IntentCategory::Communication,
                keywords: vec![
                    "邮件", "发邮件", "收邮件", "email", "Email",
                    "推特", "Twitter", "twitter", "微博",
                    "Medium", "medium", "WordPress", "wordpress", "博客",
                    "通知", "短信", "SMS", "sms", "推送", "webhook",
                ],
                patterns: vec![
                    Regex::new(r"[\w.+-]+@[\w-]+\.[\w.]+").unwrap(), // email address
                ],
                negative: vec![],
                priority: 5,
            },
            // ── SystemControl ──
            IntentRule {
                category: IntentCategory::SystemControl,
                keywords: vec![
                    "截图", "拍照", "摄像头", "相机", "屏幕",
                    "应用", "软件", "窗口", "菜单",
                    "Chrome", "chrome", "Safari", "safari", "Windsurf", "windsurf", "VSCode", "vscode",
                    "系统信息", "硬件", "CPU", "cpu", "GPU", "gpu", "内存",
                    "screenshot", "camera", "app control",
                    "Android", "android", "手机", "Termux", "termux",
                    "短信", "通话", "传感器", "GPS", "gps", "手电筒", "亮度", "音量",
                    "打开技能", "关闭技能", "启用技能", "禁用技能",
                    "打开能力", "关闭能力", "启用能力", "禁用能力",
                    "enable skill", "disable skill", "enable capability", "disable capability",
                    "toggle",
                ],
                patterns: vec![
                    Regex::new(r"(?i)(打开|开启|启用|关闭|禁用|enable|disable)\s*.{1,30}(技能|能力|skill|capability|tool)").unwrap(),
                    Regex::new(r"(?i)(打开|开启|启用|关闭|禁用|enable|disable)\s+[a-zA-Z_][a-zA-Z0-9_]*").unwrap(),
                ],
                negative: vec![],
                priority: 6,
            },
            // ── Organization ──
            IntentRule {
                category: IntentCategory::Organization,
                keywords: vec![
                    "日程", "日历", "会议", "提醒", "定时", "计划",
                    "任务", "待办", "进度", "后台",
                    "记住", "记忆", "笔记", "知识图谱",
                    "Notion", "notion", "Jira", "jira",
                    "calendar", "schedule", "reminder", "cron", "todo",
                    "安装技能", "安装skill", "下载技能", "从hub安装", "从Hub安装",
                    "install skill", "uninstall skill", "技能商店", "技能市场",
                    "hub技能", "查看技能", "已安装技能",
                ],
                patterns: vec![],
                negative: vec![],
                priority: 5,
            },
            // ── IoT ──
            IntentRule {
                category: IntentCategory::IoT,
                keywords: vec![
                    "智能家居", "家居", "灯", "空调", "温度", "湿度",
                    "Home Assistant", "home assistant", "HomeAssistant",
                    "MQTT", "mqtt", "传感器", "开关",
                    "IoT", "iot", "smart home",
                ],
                patterns: vec![],
                negative: vec![],
                priority: 5,
            },
            // ── Media ──
            IntentRule {
                category: IntentCategory::Media,
                keywords: vec![
                    "语音", "音频", "视频", "转录", "字幕", "朗读", "播放",
                    "图片", "照片", "图像",
                    "OCR", "ocr", "文字识别", "图片识别", "图片分析", "看图",
                    "剪辑", "合并视频", "水印", "缩略图", "转码",
                    "TTS", "tts", "语音合成", "whisper",
                    "transcribe", "speech", "audio", "video", "image", "photo",
                ],
                patterns: vec![
                    Regex::new(r"(?i)\.(mp3|wav|m4a|flac|ogg|mp4|mkv|webm|avi|mov)\b").unwrap(),
                ],
                negative: vec![],
                priority: 5,
            },
            // ── DevOps ──
            IntentRule {
                category: IntentCategory::DevOps,
                keywords: vec![
                    "GitHub", "github", "Git", "git", "PR", "pull request", "issue",
                    "部署", "服务器", "云服务", "云计算", "云平台", "AWS", "aws", "GCP", "gcp", "Azure", "azure",
                    "Docker", "docker", "容器", "K8s", "k8s",
                    "网络", "ping", "端口", "SSL", "ssl", "证书", "DNS", "dns",
                    "whois", "traceroute", "域名", "带宽", "网速",
                    "加密", "解密", "密码", "哈希", "hash",
                    "deploy", "server", "cloud", "container", "encrypt", "decrypt",
                ],
                patterns: vec![],
                negative: vec![],
                priority: 5,
            },
            // ── Lifestyle ──
            IntentRule {
                category: IntentCategory::Lifestyle,
                keywords: vec![
                    "健康", "步数", "心率", "睡眠", "运动", "体重",
                    "地图", "导航", "路线", "附近", "地址", "经纬度",
                    "联系人", "通讯录", "电话",
                    "health", "fitness", "map", "direction", "contact",
                ],
                patterns: vec![],
                negative: vec![],
                priority: 4,
            },
        ];

        Self { rules }
    }

    /// Classify user input into one or more intent categories.
    /// Returns up to 2 categories, sorted by priority.
    pub fn classify(&self, input: &str) -> Vec<IntentCategory> {
        let input_lower = input.to_lowercase();
        let mut matches: Vec<(IntentCategory, u8)> = Vec::new();

        for rule in &self.rules {
            if self.rule_matches(rule, input, &input_lower) {
                matches.push((rule.category.clone(), rule.priority));
            }
        }

        if matches.is_empty() {
            return vec![IntentCategory::Unknown];
        }

        // Sort by priority descending
        matches.sort_by(|a, b| b.1.cmp(&a.1));
        matches.dedup_by(|a, b| a.0 == b.0);

        // If Chat is the only match, return it alone
        if matches.len() == 1 && matches[0].0 == IntentCategory::Chat {
            return vec![IntentCategory::Chat];
        }

        // If Chat is matched alongside other intents, drop Chat
        matches.retain(|m| m.0 != IntentCategory::Chat);

        if matches.is_empty() {
            return vec![IntentCategory::Unknown];
        }

        // Take top 2
        matches.into_iter().take(2).map(|(c, _)| c).collect()
    }

    fn rule_matches(&self, rule: &IntentRule, input: &str, input_lower: &str) -> bool {
        // Check negative keywords first
        for neg in &rule.negative {
            if input_lower.contains(&neg.to_lowercase()) {
                return false;
            }
        }

        // Check regex patterns
        for pattern in &rule.patterns {
            if pattern.is_match(input) {
                return true;
            }
        }

        // Check keywords
        for keyword in &rule.keywords {
            if input_lower.contains(&keyword.to_lowercase()) {
                return true;
            }
        }

        false
    }
}

/// Get the tool names that should be loaded for a set of intents.
pub fn tools_for_intents(intents: &[IntentCategory]) -> Vec<&'static str> {
    let mut tools = HashSet::new();

    for intent in intents {
        for tool in tools_for_intent(intent) {
            tools.insert(tool);
        }
    }

    let mut result: Vec<&str> = tools.into_iter().collect();
    result.sort();
    result
}

fn tools_for_intent(intent: &IntentCategory) -> Vec<&'static str> {
    // Core tools included in all non-Chat intents
    let core: Vec<&str> = vec![
        "read_file", "write_file", "list_dir", "exec",
        "web_search", "web_fetch",
        "memory_query", "memory_upsert",
        "toggle_manage", "message",
    ];

    match intent {
        IntentCategory::Chat => vec![], // No tools at all
        IntentCategory::FileOps => {
            let mut t = core.clone();
            t.extend(["edit_file", "file_ops", "data_process", "office_write"]);
            t
        }
        IntentCategory::WebSearch => {
            let mut t = core.clone();
            t.extend(["browse", "http_request"]);
            t
        }
        IntentCategory::Finance => {
            let mut t = core.clone();
            t.extend([
                "finance_api", "exchange_api", "http_request", "data_process",
                "chart_generate", "alert_rule", "stream_subscribe", "notification",
                "knowledge_graph", "cron", "office_write", "browse",
            ]);
            t
        }
        IntentCategory::Blockchain => {
            let mut t = core.clone();
            t.extend([
                "finance_api", "blockchain_rpc", "blockchain_tx", "contract_security",
                "bridge_api", "nft_market", "multisig", "exchange_api",
                "stream_subscribe", "http_request", "knowledge_graph",
            ]);
            t
        }
        IntentCategory::DataAnalysis => {
            let mut t = core.clone();
            t.extend([
                "edit_file", "file_ops", "data_process", "chart_generate",
                "office_write", "http_request",
            ]);
            t
        }
        IntentCategory::Communication => {
            let mut t = core.clone();
            t.extend([
                "email", "social_media", "notification", "message",
                "http_request", "community_hub",
            ]);
            t
        }
        IntentCategory::SystemControl => {
            let mut t = core.clone();
            t.extend([
                "system_info", "capability_evolve", "app_control",
                "camera_capture", "browse",
                "image_understand", "termux_api",
            ]);
            t
        }
        IntentCategory::Organization => {
            let mut t = core.clone();
            t.extend([
                "calendar_api", "cron", "memory_forget",
                "knowledge_graph", "list_tasks", "spawn", "list_skills",
                "memory_maintenance", "community_hub",
            ]);
            t
        }
        IntentCategory::IoT => {
            let mut t = core.clone();
            t.extend(["iot_control", "http_request", "notification", "cron"]);
            t
        }
        IntentCategory::Media => {
            let mut t = core.clone();
            t.extend([
                "audio_transcribe", "tts", "ocr", "image_understand",
                "video_process", "file_ops", "notification",
            ]);
            t
        }
        IntentCategory::DevOps => {
            let mut t = core.clone();
            t.extend([
                "git_api", "cloud_api", "network_monitor", "encrypt",
                "http_request", "edit_file", "file_ops",
            ]);
            t
        }
        IntentCategory::Lifestyle => {
            let mut t = core.clone();
            t.extend([
                "health_api", "map_api", "contacts", "http_request",
            ]);
            t
        }
        IntentCategory::Unknown => {
            // Core + high-frequency tools (~15)
            let mut t = core.clone();
            t.extend([
                "edit_file", "file_ops", "office_write", "http_request", "browse",
                "spawn", "list_tasks", "cron", "notification",
                "memory_forget", "list_skills",
                "community_hub", "memory_maintenance",
            ]);
            t
        }
    }
}

/// Check if the intents require financial domain knowledge injection.
pub fn needs_finance_guidelines(intents: &[IntentCategory]) -> bool {
    intents.iter().any(|i| matches!(i, IntentCategory::Finance | IntentCategory::Blockchain))
}

/// Check if the intents should show skills list.
pub fn needs_skills_list(intents: &[IntentCategory]) -> bool {
    !intents.iter().any(|i| matches!(i, IntentCategory::Chat))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chat_classification() {
        let classifier = IntentClassifier::new();
        assert_eq!(classifier.classify("你好"), vec![IntentCategory::Chat]);
        assert_eq!(classifier.classify("hello"), vec![IntentCategory::Chat]);
        assert_eq!(classifier.classify("Hi!"), vec![IntentCategory::Chat]);
        assert_eq!(classifier.classify("谢谢"), vec![IntentCategory::Chat]);
        assert_eq!(classifier.classify("再见"), vec![IntentCategory::Chat]);
        assert_eq!(classifier.classify("你是谁?"), vec![IntentCategory::Chat]);
    }

    #[test]
    fn test_finance_classification() {
        let classifier = IntentClassifier::new();
        let intents = classifier.classify("查一下茅台股价");
        assert!(intents.contains(&IntentCategory::Finance));

        let intents = classifier.classify("601318 最近行情怎么样");
        assert!(intents.contains(&IntentCategory::Finance));

        let intents = classifier.classify("BTC价格多少");
        assert!(intents.contains(&IntentCategory::Finance));

        // 云天化 should match Finance (涨停), NOT DevOps (云 was a false positive)
        let intents = classifier.classify("分析股票云天化近期涨停原因");
        assert!(intents.contains(&IntentCategory::Finance));
        assert!(!intents.contains(&IntentCategory::DevOps), "云天化 should not trigger DevOps");
    }

    #[test]
    fn test_blockchain_classification() {
        let classifier = IntentClassifier::new();
        let intents = classifier.classify("0x1234567890abcdef1234567890abcdef12345678 这个地址安全吗");
        assert!(intents.contains(&IntentCategory::Blockchain));

        let intents = classifier.classify("帮我查一下这个智能合约");
        assert!(intents.contains(&IntentCategory::Blockchain));
    }

    #[test]
    fn test_file_ops_classification() {
        let classifier = IntentClassifier::new();
        let intents = classifier.classify("帮我读一下 config.json");
        assert!(intents.contains(&IntentCategory::FileOps));

        let intents = classifier.classify("创建一个 test.py 文件");
        assert!(intents.contains(&IntentCategory::FileOps));
    }

    #[test]
    fn test_unknown_classification() {
        let classifier = IntentClassifier::new();
        let intents = classifier.classify("帮我做一件复杂的事情");
        assert_eq!(intents, vec![IntentCategory::Unknown]);
    }

    #[test]
    fn test_multi_intent() {
        let classifier = IntentClassifier::new();
        // "查一下茅台股价然后生成图表" should match Finance + DataAnalysis
        let intents = classifier.classify("查一下茅台股价然后生成图表");
        assert!(!intents.is_empty());
        assert!(intents.contains(&IntentCategory::Finance));
    }

    #[test]
    fn test_tools_for_chat() {
        let tools = tools_for_intents(&[IntentCategory::Chat]);
        assert!(tools.is_empty());
    }

    #[test]
    fn test_tools_for_finance() {
        let tools = tools_for_intents(&[IntentCategory::Finance]);
        assert!(tools.contains(&"finance_api"));
        assert!(tools.contains(&"exchange_api"));
        assert!(tools.contains(&"read_file")); // core tool
    }

    #[test]
    fn test_tools_for_unknown() {
        let tools = tools_for_intents(&[IntentCategory::Unknown]);
        assert!(tools.contains(&"read_file"));
        assert!(tools.contains(&"browse"));
    }
}
