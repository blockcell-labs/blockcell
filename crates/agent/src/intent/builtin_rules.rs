//! 意图分类器的内置规则表。
//!
//! 数百行 `IntentRule` 字面量从 `intent.rs` 抽出，避免主文件被静态数据淹没。
//! `IntentClassifier::new()` 调用 `builtin_rules()` 取得这张表后再做归一化预处理。

use regex::Regex;

use super::{IntentCategory, IntentRule};

/// 返回内置意图规则表（静态数据）。
pub(super) fn builtin_rules() -> Vec<IntentRule> {
    vec![
            // ── Coding (priority 80) ──
            IntentRule {
                category: IntentCategory::Coding,
                keywords: vec![
                    "修复代码", "修改代码", "创建代码", "实现功能", "新增功能", "代码重构", "调试代码", "修好",
                    "写测试", "跑测试", "运行测试", "编译项目", "构建项目", "代码审查",
                    "fix bug", "fix the bug", "modify code", "edit code", "create code", "implement feature",
                    "add feature", "refactor", "debug code", "write tests", "run tests",
                    "code review", "cargo test", "go test", "pytest", "npm test", "ci",
                ],
                patterns: vec![
                    Regex::new(r"(?i)(修复|修改|编辑|创建|实现|新增|重构|调试|测试|编译|构建|fix|modify|edit|create|implement|add|refactor|debug|test|build).{0,80}([A-Za-z0-9_./\\-]+\.(rs|py|go|js|jsx|ts|tsx|java|kt|c|cc|cpp|h|hpp|rb|php|swift|toml|yaml|yml|json|md|sh)|\b(cargo|npm|pnpm|yarn|pytest|make|cmake)\b)").unwrap(),
                    Regex::new(r"(?i)\b(cargo\s+(test|build|check|clippy)|go\s+test|pytest|npm\s+(test|run\s+(build|lint)))\b").unwrap(),
                ],
                negative: vec!["解释", "介绍", "是什么", "如何", "怎么做", "what is", "how to"],
                keywords_dyn: vec![],
                negative_dyn: vec![],
                keywords_lower: vec![],
                negative_lower: vec![],
                priority: 80,
            },
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
                keywords_dyn: vec![],
                negative_dyn: vec![],
                keywords_lower: vec![],
                negative_lower: vec![],
                priority: 10,
            },
            // ── Finance (priority 65) ──
            IntentRule {
                category: IntentCategory::Finance,
                keywords: vec![
                    "股价", "行情", "涨跌", "k线", "市值", "etf", "基金", "期货",
                    "股票", "买入", "卖出", "仓位", "盈亏", "止损", "市盈率", "分红",
                    "stock", "trading", "portfolio", "market cap", "fund", "futures",
                    "dividend", "bull market", "bear market", "shares",
                    "a股", "港股", "美股", "纳斯达克", "道琼斯", "上证", "深证", "沪深",
                ],
                patterns: vec![
                    Regex::new(r"(?i)\b(stock\s*price|market\s*cap|p/e\s*ratio|pe\s*ratio)\b").unwrap(),
                    Regex::new(r"\d+(\.\d+)?\s*(元|美元|港元|点位)").unwrap(),
                ],
                negative: vec![],
                keywords_dyn: vec![],
                negative_dyn: vec![],
                keywords_lower: vec![],
                negative_lower: vec![],
                priority: 65,
            },
            // ── Blockchain (priority 65) ──
            IntentRule {
                category: IntentCategory::Blockchain,
                keywords: vec![
                    "区块链", "链上", "钱包", "合约", "nft", "代币", "挖矿", "gas费",
                    "转账", "defi", "dao", "公链", "私钥", "助记词",
                    "blockchain", "crypto", "bitcoin", "ethereum", "solana",
                    "wallet", "token", "mining",
                ],
                patterns: vec![
                    Regex::new(r"0x[0-9a-fA-F]{40}").unwrap(),
                    Regex::new(r"(?i)\b(BTC|ETH|BNB|SOL|USDT|USDC|MATIC|AVAX)\b").unwrap(),
                ],
                negative: vec![],
                keywords_dyn: vec![],
                negative_dyn: vec![],
                keywords_lower: vec![],
                negative_lower: vec![],
                priority: 65,
            },
            // ── FileOps (priority 55) - 通用文件操作，优先级低于专用类别 ──
            IntentRule {
                category: IntentCategory::FileOps,
                keywords: vec![
                    "读文件", "写文件", "创建文件", "删除文件", "列目录", "列出文件",
                    "重命名", "复制文件", "移动文件",
                    "read file", "write file", "create file", "delete file",
                    "list dir", "rename file",
                ],
                patterns: vec![
                    // 扩展名匹配：要求前面是空格或行首，排除 URL 路径中的扩展名
                    Regex::new(r"(?i)(^|\s)[a-zA-Z0-9_\-]+\.(rs|py|go|js|ts|json|json5|toml|yaml|yml|md|txt|sh|log|conf|cfg|ini|lock)\b").unwrap(),
                    // 操作 + 文件关键词（允许中间词）
                    Regex::new(r"(?i)(read|write|edit|create|delete|rename|copy|move|list|cat|ls|mkdir|rm|cp|mv|touch|chmod).*?(file|directory|folder|dir|文件|目录|文件夹)").unwrap(),
                    // 中文操作 + 文件（允许中间词）
                    Regex::new(r"(读|写|编辑|创建|删除|重命名|复制|移动|列出).*?(文件|目录|文件夹)").unwrap(),
                    // 命令行工具
                    Regex::new(r"(?i)\b(cat|ls|mkdir|rm|cp|mv|touch|chmod)\s+").unwrap(),
                ],
                negative: vec![
                    // 排除 IoT 设备控制关键词
                    "灯", "空调", "窗帘", "风扇", "暖气", "热水器",
                    // 排除 SystemControl 应用控制关键词
                    "应用", "软件", "程序", "app",
                    // 排除 Media 媒体关键词
                    "mp3", "mp4", "wav", "avi", "mkv", "jpg", "jpeg", "png", "gif", "webp",
                ],
                keywords_dyn: vec![],
                negative_dyn: vec![],
                keywords_lower: vec![],
                negative_lower: vec![],
                priority: 55,
            },
            // ── WebSearch (priority 55) ──
            IntentRule {
                category: IntentCategory::WebSearch,
                keywords: vec![
                    "搜索", "查一下", "查询", "找一找", "查找", "搜一搜", "百度", "谷歌", "网上找",
                    "search", "google", "bing", "look up", "find out", "browse",
                ],
                patterns: vec![
                    Regex::new(r"(?i)\b(what\s+is|how\s+to|where\s+is|when\s+did|who\s+is)\b").unwrap(),
                    Regex::new(r"(?i)(网上|网页|互联网|internet|web)\s*(搜|找|查|看)").unwrap(),
                ],
                negative: vec![
                    // 排除金融关键词：股价行情搜索应触发 Finance 而非 WebSearch
                    "股价", "行情", "股票", "币", "币价", "stock price", "币种",
                ],
                keywords_dyn: vec![],
                negative_dyn: vec![],
                keywords_lower: vec![],
                negative_lower: vec![],
                priority: 55,
            },
            // ── DataAnalysis (priority 60) ──
            IntentRule {
                category: IntentCategory::DataAnalysis,
                keywords: vec![
                    "数据分析", "图表", "可视化", "统计", "报表", "画图",
                    "折线图", "柱状图", "饼图", "散点图",
                    "analyze", "chart", "graph", "plot", "visualize",
                    "statistics", "report", "dashboard",
                ],
                patterns: vec![
                    Regex::new(r"(?i)(数据|data)\s*(处理|分析|清洗|转换|导出|挖掘)").unwrap(),
                    Regex::new(r"(?i)(生成|绘制|画)\s*(图|表|报告)").unwrap(),
                    // 数据文件扩展名（单独匹配，不与其他文件操作词组合）
                    Regex::new(r"(?i)\b\.(csv|xlsx|xls|parquet)\b").unwrap(),
                    Regex::new(r"(?i)(分析|处理)\s*.*?\.(csv|xlsx|xls|parquet)").unwrap(),
                ],
                negative: vec![],
                keywords_dyn: vec![],
                negative_dyn: vec![],
                keywords_lower: vec![],
                negative_lower: vec![],
                priority: 60,
            },
            // ── Communication (priority 60) ──
            IntentRule {
                category: IntentCategory::Communication,
                keywords: vec![
                    "发邮件", "发消息", "发短信", "通知", "群发", "回复消息", "发送邮件",
                    "send email", "send message", "notify", "email to",
                ],
                patterns: vec![
                    Regex::new(r"(?i)(发送|send|写|写一封)\s*(邮件|email|消息|message|通知|notification)").unwrap(),
                    Regex::new(r"(?i)(email|邮件)\s*(给|to)\s*[a-zA-Z0-9._%+\-]+@[a-zA-Z0-9.\-]+\.[a-zA-Z]{2,}").unwrap(),
                ],
                negative: vec![
                    // 排除提到邮箱地址但不是要发邮件的场景
                    "邮箱是", "我的邮箱", "联系我", "email is", "my email",
                ],
                keywords_dyn: vec![],
                negative_dyn: vec![],
                keywords_lower: vec![],
                negative_lower: vec![],
                priority: 60,
            },
            // ── SystemControl (priority 65) - 系统控制优先级高于通用 FileOps ──
            IntentRule {
                category: IntentCategory::SystemControl,
                keywords: vec![
                    "系统信息", "cpu", "内存", "磁盘", "进程", "截图", "相机", "拍照",
                    "打开应用", "关闭应用", "系统状态", "cpu使用率", "打开微信",
                    "system info", "cpu usage", "disk space",
                    "process", "screenshot", "camera",
                ],
                patterns: vec![
                    Regex::new(r"(?i)(打开|关闭|重启|安装|卸载)\s*(应用|软件|程序|app|微信|qq|浏览器)").unwrap(),
                    Regex::new(r"(?i)(系统|system)\s*(负载|使用率|状态|监控)").unwrap(),
                    Regex::new(r"(?i)查看.*?(cpu|内存|磁盘|进程)").unwrap(),
                ],
                negative: vec![],
                keywords_dyn: vec![],
                negative_dyn: vec![],
                keywords_lower: vec![],
                negative_lower: vec![],
                priority: 65,
            },
            // ── Organization (priority 55) ──
            IntentRule {
                category: IntentCategory::Organization,
                keywords: vec![
                    "定时", "提醒", "日程", "任务", "计划", "待办", "cron", "记住",
                    "记录", "备忘", "记事",
                    "remind me", "schedule task", "todo list", "calendar event",
                ],
                patterns: vec![
                    Regex::new(r"(?i)(设置|创建|添加)\s*(提醒|任务|日程|闹钟)").unwrap(),
                    Regex::new(r"\d+\s*(分钟|小时|天|周)\s*(后|内|提醒)").unwrap(),
                    Regex::new(r"(?i)(every|每)\s*(day|天|hour|小时|week|周)").unwrap(),
                ],
                negative: vec![],
                keywords_dyn: vec![],
                negative_dyn: vec![],
                keywords_lower: vec![],
                negative_lower: vec![],
                priority: 55,
            },
            // ── IoT (priority 65) - 设备控制优先级高于通用 FileOps ──
            IntentRule {
                category: IntentCategory::IoT,
                keywords: vec![
                    "iot", "智能家居", "传感器", "设备控制", "mqtt",
                    "温度计", "湿度", "灯光",
                    "smart home", "sensor", "temperature", "humidity",
                    "thermostat", "zigbee",
                ],
                patterns: vec![
                    // 允许中间词如 "客厅的灯"
                    Regex::new(r"(?i)(打开|关闭|调节).*?(灯|空调|窗帘|风扇|暖气|热水器)").unwrap(),
                    Regex::new(r"(?i)\b(mqtt|zigbee|z-wave|homeassistant|home\s*assistant)\b").unwrap(),
                ],
                negative: vec![],
                keywords_dyn: vec![],
                negative_dyn: vec![],
                keywords_lower: vec![],
                negative_lower: vec![],
                priority: 65,
            },
            // ── Media (priority 65) - 媒体处理优先级高于通用 FileOps ──
            IntentRule {
                category: IntentCategory::Media,
                keywords: vec![
                    "语音转文字", "文字转语音", "ocr", "识图", "图片理解",
                    "视频处理", "音频", "转写", "字幕",
                    "transcribe", "tts", "text to speech", "image recognition",
                    "video process", "audio",
                ],
                patterns: vec![
                    // 允许中间词如 "这张图片里的文字"
                    Regex::new(r"(?i)(识别|提取|转换|处理).*?(图片|图像|音频|视频|文字|语音)").unwrap(),
                    // 媒体文件扩展名：要求前面是空格或行首，排除 URL 和陈述事实场景
                    Regex::new(r"(?i)(^|\s)[a-zA-Z0-9_\-]+\.(mp3|mp4|wav|avi|mkv|jpg|jpeg|png|gif|webp)\b").unwrap(),
                    Regex::new(r"(?i)(语音|voice|audio)\s*(识别|转文|to\s*text)").unwrap(),
                    Regex::new(r"(?i)(把|将).*?(音频|视频|语音).*?(转成|转换为).*?(文字|文本)").unwrap(),
                ],
                negative: vec![
                    // 排除陈述事实而非请求操作的关键词
                    "下载了", "上传了", "保存了", "收到了", "watched", "saved",
                ],
                keywords_dyn: vec![],
                negative_dyn: vec![],
                keywords_lower: vec![],
                negative_lower: vec![],
                priority: 65,
            },
            // ── DevOps (priority 65) - 运维优先级高于通用 FileOps ──
            IntentRule {
                category: IntentCategory::DevOps,
                keywords: vec![
                    "部署", "运维", "监控", "端口", "加密", "解密",
                    "哈希", "证书", "ssh", "docker", "kubernetes", "k8s",
                    "deploy", "devops", "encrypt", "decrypt",
                    "hash", "certificate", "firewall",
                ],
                patterns: vec![
                    Regex::new(r"(?i)\b(GET|POST|PUT|DELETE|PATCH)\s+https?://").unwrap(),
                    Regex::new(r"(?i)\b(ping|curl|wget|nmap|ssh|scp)\s+\S").unwrap(),
                    Regex::new(r"(?i)\b(docker|kubectl|helm)\s+(run|build|push|deploy|apply)").unwrap(),
                ],
                negative: vec![
                    // 排除问答句式：询问命令是什么
                    "是什么", "什么意思", "怎么用", "怎么写", "how to use", "what is",
                ],
                keywords_dyn: vec![],
                negative_dyn: vec![],
                keywords_lower: vec![],
                negative_lower: vec![],
                priority: 65,
            },
            // ── Lifestyle (priority 50) ──
            IntentRule {
                category: IntentCategory::Lifestyle,
                keywords: vec![
                    "健康", "运动", "饮食", "卡路里", "跑步", "睡眠",
                    "天气", "菜谱", "旅游", "生活", "体重", "减肥",
                    "health", "exercise", "diet", "calories", "sleep",
                    "weather", "recipe", "travel",
                ],
                patterns: vec![
                    Regex::new(r"(?i)(今天|明天|后天)\s*(天气|气温|下雨|温度)").unwrap(),
                    Regex::new(r"(?i)(推荐|建议)\s*(菜|食谱|运动|健身)").unwrap(),
                ],
                negative: vec![],
                keywords_dyn: vec![],
                negative_dyn: vec![],
                keywords_lower: vec![],
                negative_lower: vec![],
                priority: 50,
            },
    ]
}
