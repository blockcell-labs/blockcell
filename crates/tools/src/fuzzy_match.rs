//! 模糊查找替换引擎
//!
//! 移植 Hermes 的 `tools/fuzzy_match.py`, 使用 Rust 重写, 保留完整的 9 策略链。
//! 容忍空白/缩进/Unicode 差异的 find-and-replace, 用于 Skill Patch 操作。

use std::collections::HashMap;

/// 匹配错误类型
#[derive(Debug, thiserror::Error)]
pub enum MatchError {
    #[error("old_string is empty")]
    EmptyOldString,
    #[error("old_string and new_string are identical")]
    IdenticalStrings,
    #[error("found {count} matches; provide more context or use replace_all=true")]
    MultipleMatches { count: usize, hint: String },
    #[error("no match found for old_string (preview: {content_preview})")]
    NoMatch {
        old_string: String,
        content_preview: String,
    },
}

/// 匹配结果: (起始字节偏移, 结束字节偏移)
#[derive(Debug, Clone, Copy)]
struct MatchSpan {
    /// 在原始 content 中的起始偏移
    orig_start: usize,
    /// 在原始 content 中的结束偏移
    orig_end: usize,
}

/// 策略函数类型: 接收 (content, old_string), 返回匹配列表
type StrategyFn = fn(&str, &str) -> Vec<MatchSpan>;

/// 模糊查找替换: 容忍空白/缩进/Unicode 差异的 find-and-replace
///
/// 策略链 (按顺序尝试, 参考 Hermes tools/fuzzy_match.py):
/// 1. exact           - 精确匹配
/// 2. line_trimmed    - 逐行 strip 前尾空白
/// 3. whitespace_norm - 多空格/Tab 压缩为单空格
/// 4. indent_flex     - 忽略缩进差异
/// 5. escape_norm     - \n 字面量 → 真换行
/// 6. trim_boundary   - 只 strip 首尾行
/// 7. unicode_norm    - 智能引号/em-dash/ellipsis → ASCII
/// 8. block_anchor    - 首尾行锚定 + 中间行相似度
/// 9. context_aware   - 逐行 80% 相似度, 整体 ≥50%
///
/// 多匹配处理: replace_all=true 时替换所有, 否则要求唯一性
///
/// 返回: (新内容, 替换次数, 使用的策略名)
pub fn fuzzy_find_and_replace(
    content: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> Result<(String, usize, String), MatchError> {
    if old_string.is_empty() {
        return Err(MatchError::EmptyOldString);
    }
    if old_string == new_string {
        return Err(MatchError::IdenticalStrings);
    }

    let strategies: &[(&str, StrategyFn)] = &[
        ("exact", strategy_exact),
        ("line_trimmed", strategy_line_trimmed),
        ("whitespace_norm", strategy_whitespace_normalized),
        ("indent_flex", strategy_indentation_flexible),
        ("escape_norm", strategy_escape_normalized),
        ("trim_boundary", strategy_trimmed_boundary),
        ("unicode_norm", strategy_unicode_normalized),
        ("block_anchor", strategy_block_anchor),
        ("context_aware", strategy_context_aware),
    ];

    for (name, strategy_fn) in strategies {
        let matches = strategy_fn(content, old_string);

        if !matches.is_empty() {
            if matches.len() > 1 && !replace_all {
                return Err(MatchError::MultipleMatches {
                    count: matches.len(),
                    hint: "Provide more context to make it unique, or use replace_all=true"
                        .to_string(),
                });
            }

            let new_content = apply_replacements(content, &matches, new_string);
            return Ok((new_content, matches.len(), name.to_string()));
        }
    }

    Err(MatchError::NoMatch {
        old_string: old_string.to_string(),
        content_preview: content.chars().take(500).collect(),
    })
}

// ── 策略 1: 精确匹配 ──

fn strategy_exact(content: &str, old_string: &str) -> Vec<MatchSpan> {
    find_all_occurrences(content, old_string)
}

/// 在 content 中查找所有 old_string 的出现位置
fn find_all_occurrences(content: &str, old_string: &str) -> Vec<MatchSpan> {
    let mut spans = Vec::new();
    let mut start = 0;
    while let Some(pos) = content[start..].find(old_string) {
        let abs_start = start + pos;
        let abs_end = abs_start + old_string.len();
        spans.push(MatchSpan {
            orig_start: abs_start,
            orig_end: abs_end,
        });
        start = abs_end;
    }
    spans
}

// ── 策略 2: 逐行 strip (行号映射, 避免偏移错误) ──

fn strategy_line_trimmed(content: &str, old_string: &str) -> Vec<MatchSpan> {
    find_normalized_with_line_mapping(content, old_string, normalize_line_trimmed)
}

fn normalize_line_trimmed(s: &str) -> String {
    s.lines()
        .map(|line| line.trim())
        .collect::<Vec<_>>()
        .join("\n")
}

// ── 策略 3: 空白压缩 (行号映射) ──

fn strategy_whitespace_normalized(content: &str, old_string: &str) -> Vec<MatchSpan> {
    find_normalized_with_line_mapping(content, old_string, normalize_whitespace)
}

fn normalize_whitespace(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut prev_was_space = false;
    for ch in s.chars() {
        if ch == ' ' || ch == '\t' {
            if !prev_was_space {
                result.push(' ');
                prev_was_space = true;
            }
        } else if ch == '\n' {
            result.push('\n');
            prev_was_space = false;
        } else {
            result.push(ch);
            prev_was_space = false;
        }
    }
    result
}

// ── 策略 4: 缩进弹性 (行号映射) ──

fn strategy_indentation_flexible(content: &str, old_string: &str) -> Vec<MatchSpan> {
    find_normalized_with_line_mapping(content, old_string, normalize_indentation)
}

fn normalize_indentation(s: &str) -> String {
    s.lines()
        .map(|line| line.trim_start())
        .collect::<Vec<_>>()
        .join("\n")
}

// ── 策略 5: 转义归一化 (行号映射, 无转义变化时跳过) ──

fn strategy_escape_normalized(content: &str, old_string: &str) -> Vec<MatchSpan> {
    // 优化: 如果 old_string 不含转义序列, 跳过此策略
    let normalized_old = normalize_escapes(old_string);
    if normalized_old == old_string {
        return Vec::new();
    }
    // normalize_escapes 可能将 \n 字面量转为真实换行, 改变行数,
    // 这会破坏 find_normalized_with_line_mapping 的行号映射假设。
    // 使用直接偏移映射: 在整体规范化文本上匹配, 然后通过
    // 逐字符对齐将规范化偏移映射回原始偏移。
    find_normalized_with_offset_mapping(content, old_string, normalize_escapes)
}

fn normalize_escapes(s: &str) -> String {
    s.replace("\\n", "\n")
        .replace("\\t", "\t")
        .replace("\\r", "\r")
}

// ── 策略 6: 首尾行 strip (行号映射) ──

fn strategy_trimmed_boundary(content: &str, old_string: &str) -> Vec<MatchSpan> {
    find_normalized_with_line_mapping(content, old_string, normalize_trimmed_boundary)
}

fn normalize_trimmed_boundary(s: &str) -> String {
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() <= 2 {
        return s.trim().to_string();
    }
    let first = lines.first().unwrap().trim();
    let last = lines.last().unwrap().trim();
    let middle = &lines[1..lines.len() - 1];
    format!("{}\n{}\n{}", first, middle.join("\n"), last)
}

// ── 策略 7: Unicode 归一化 (行号映射) ──

/// Unicode 映射表: 智能引号/em-dash/ellipsis → ASCII
const UNICODE_MAP: &[(&str, &str)] = &[
    ("\u{201c}", "\""),  // 左双引号 "
    ("\u{201d}", "\""),  // 右双引号 "
    ("\u{2018}", "'"),   // 左单引号 '
    ("\u{2019}", "'"),   // 右单引号 '
    ("\u{2013}", "-"),   // en-dash –
    ("\u{2014}", "--"),  // em-dash —
    ("\u{2026}", "..."), // ellipsis …
    ("\u{00a0}", " "),   // non-breaking space
];

fn strategy_unicode_normalized(content: &str, old_string: &str) -> Vec<MatchSpan> {
    // 第一轮: 在 Unicode 规范化文本上精确匹配
    let result = find_normalized_with_line_mapping(content, old_string, normalize_unicode);
    if !result.is_empty() {
        return result;
    }

    // 第二轮回退: 在 Unicode 规范化 + 行首尾空白去除后匹配
    find_normalized_with_line_mapping(content, old_string, |s| {
        normalize_unicode(&normalize_line_trimmed(s))
    })
}

fn normalize_unicode(s: &str) -> String {
    let mut result = s.to_string();
    for (from, to) in UNICODE_MAP {
        result = result.replace(from, to);
    }
    result
}

// ── 行号映射: 在规范化文本上匹配, 通过行号映射回原始文本偏移 ──
//
// 修复策略 2-7 的位置映射 BUG:
// 规范化会改变文本长度 (如缩进删除、空白压缩、Unicode 扩展),
// 直接用规范化文本偏移在原始文本上操作会导致替换位置错误。
// 解决方案: 在规范化文本上匹配, 通过行号映射回原始文本的字节偏移。

fn find_normalized_with_line_mapping(
    content: &str,
    old_string: &str,
    normalize_fn: impl Fn(&str) -> String,
) -> Vec<MatchSpan> {
    let content_lines: Vec<&str> = content.lines().collect();
    #[allow(clippy::redundant_closure)]
    let norm_content_lines: Vec<String> = content_lines.iter().map(|l| normalize_fn(l)).collect();
    #[allow(clippy::redundant_closure)]
    let norm_old_lines: Vec<String> = old_string.lines().map(|l| normalize_fn(l)).collect();

    let norm_content = norm_content_lines.join("\n");
    let norm_old = norm_old_lines.join("\n");

    // 在规范化文本上查找
    let norm_matches = find_all_occurrences(&norm_content, &norm_old);

    // 通过行号映射回原始文本偏移
    norm_matches
        .iter()
        .filter_map(|span| {
            // 计算匹配起始位置对应的行号
            let start_line = norm_content[..span.orig_start].lines().count();
            let end_line = start_line + norm_old_lines.len();
            if end_line > content_lines.len() {
                return None;
            }

            // 从行号计算原始文本的字节偏移
            let orig_start = line_byte_offset(content, start_line);
            let orig_end =
                line_byte_offset(content, end_line - 1) + content_lines[end_line - 1].len();
            Some(MatchSpan {
                orig_start,
                orig_end,
            })
        })
        .collect()
}

/// 偏移映射: 在规范化文本上匹配, 通过偏移映射表映射回原始偏移
///
/// 与 find_normalized_with_line_mapping 不同, 此函数不假设规范化保持行数不变,
/// 适用于 normalize_escapes 等可能改变行数的规范化函数。
///
/// 针对 normalize_escapes 的专用实现:
/// - 逐字节扫描 content, 识别转义序列 (\n, \t, \r)
/// - 转义序列在原始中占 2 字节, 在规范化后占 1 字节
/// - 建立规范化偏移 → 原始偏移的映射表
/// - 在规范化文本上查找匹配, 然后通过映射表映射回原始偏移
fn find_normalized_with_offset_mapping(
    content: &str,
    old_string: &str,
    normalize_fn: impl Fn(&str) -> String,
) -> Vec<MatchSpan> {
    let norm_content = normalize_fn(content);
    let norm_old = normalize_fn(old_string);

    // 在规范化文本上查找
    let norm_matches = find_all_occurrences(&norm_content, &norm_old);

    if norm_matches.is_empty() {
        return Vec::new();
    }

    // 建立偏移映射: norm_to_orig[i] = 原始 content 中对应字节位置 i 的偏移
    // 含义: 规范化文本的前 i 个字节对应原始文本的前 norm_to_orig[i] 个字节
    let norm_to_orig = build_escape_offset_mapping(content, &norm_content);

    // 将匹配偏移映射回原始偏移
    norm_matches
        .iter()
        .filter_map(|span| {
            if span.orig_start >= norm_to_orig.len() || span.orig_end >= norm_to_orig.len() {
                return None;
            }
            let orig_start = norm_to_orig[span.orig_start];
            let orig_end = norm_to_orig[span.orig_end];
            if orig_start >= orig_end || orig_end > content.len() {
                return None;
            }
            Some(MatchSpan {
                orig_start,
                orig_end,
            })
        })
        .collect()
}

/// 建立规范化偏移 → 原始偏移的映射表
///
/// 逐字节扫描 content, 跟踪转义序列 (\n → 真换行, \t → 真 Tab, \r → 真 CR):
/// - 转义序列: 原始 2 字节 → 规范化 1 字节
/// - 普通字符: 原始 N 字节 → 规范化 N 字节 (N = UTF-8 字符字节数)
///
/// 返回 Vec<usize>, 长度 = norm_content.len() + 1,
/// 其中 mapping[i] 表示规范化文本前 i 个字节对应原始文本前 mapping[i] 个字节
fn build_escape_offset_mapping(content: &str, _norm_content: &str) -> Vec<usize> {
    let bytes = content.as_bytes();
    let mut mapping = Vec::with_capacity(bytes.len() + 1);

    let mut orig_pos = 0usize;
    let mut norm_pos = 0usize;

    // mapping[0] = 0: 规范化位置 0 对应原始位置 0
    mapping.push(0);

    while orig_pos < bytes.len() {
        // 检查转义序列: \n, \t, \r (backslash + n/t/r)
        if orig_pos + 1 < bytes.len() && bytes[orig_pos] == b'\\' {
            let next = bytes[orig_pos + 1];
            if next == b'n' || next == b't' || next == b'r' {
                // 转义序列: 原始 2 字节 → 规范化 1 字节
                orig_pos += 2;
                norm_pos += 1;
                mapping.push(orig_pos);
                continue;
            }
        }

        // 普通字符: 原始和规范化占相同字节数
        let char_len = utf8_char_len(bytes[orig_pos]);
        orig_pos += char_len;
        norm_pos += char_len;
        // 每个规范化字节都映射到相同的原始位置
        for _ in 0..char_len {
            mapping.push(orig_pos);
        }
    }

    // 确保 mapping 覆盖末尾 (norm_content.len() 位置)
    // mapping[norm_content.len()] 应映射到 content.len()
    while mapping.len() <= norm_pos {
        mapping.push(content.len());
    }

    mapping
}

/// 从 UTF-8 首字节确定字符的字节长度
fn utf8_char_len(first_byte: u8) -> usize {
    if first_byte < 0x80 {
        1
    } else if first_byte < 0xE0 {
        2
    } else if first_byte < 0xF0 {
        3
    } else {
        4
    }
}

// ── 策略 8: 首尾行锚定 (动态阈值: 单候选 0.50, 多候选 0.70) ──

fn strategy_block_anchor(content: &str, old_string: &str) -> Vec<MatchSpan> {
    let content_lines: Vec<&str> = content.lines().collect();
    let old_lines: Vec<&str> = old_string.lines().collect();

    if old_lines.len() < 2 || old_lines.len() > content_lines.len() {
        return Vec::new(); // 需要至少 2 行且不能超过内容行数
    }

    let first_old = old_lines.first().unwrap().trim();
    let last_old = old_lines.last().unwrap().trim();

    // 第一轮: 用低阈值 (0.50) 收集所有候选
    let low_threshold = 0.50;
    let high_threshold = 0.70;

    // 候选: (首行索引, 尾行索引, 中间行最小相似度)
    let mut candidates: Vec<(usize, usize, f64)> = Vec::new();

    // 查找首行匹配
    for i in 0..content_lines.len() {
        if content_lines[i].trim() != first_old {
            continue;
        }

        // 查找尾行匹配 (从 i + old_lines.len() - 1 附近)
        let expected_last_idx = i + old_lines.len() - 1;
        if expected_last_idx >= content_lines.len() {
            continue;
        }
        let search_range_start = expected_last_idx.saturating_sub(2);
        let search_range_end = (expected_last_idx + 3).min(content_lines.len());

        for j in search_range_start..search_range_end {
            if j <= i || content_lines[j].trim() != last_old {
                continue;
            }

            // 中间行相似度检查
            if j <= i + 1 {
                // 没有中间行, 仅首尾行匹配即可
                candidates.push((i, j, 1.0));
                continue;
            }

            let middle_content = &content_lines[i + 1..j];
            let middle_old = &old_lines[1..old_lines.len() - 1];

            if middle_content.len() != middle_old.len() {
                continue;
            }

            // 逐行比较, 记录最小相似度
            let mut min_sim = 1.0;
            for (cl, ol) in middle_content.iter().zip(middle_old.iter()) {
                let sim = line_similarity(cl.trim(), ol.trim());
                if sim < min_sim {
                    min_sim = sim;
                }
            }

            if min_sim >= low_threshold {
                candidates.push((i, j, min_sim));
            }
        }
    }

    if candidates.is_empty() {
        return Vec::new();
    }

    // 动态阈值: 多候选时提高阈值以减少误匹配
    let threshold = if candidates.len() > 1 {
        high_threshold
    } else {
        low_threshold
    };

    // 第二轮: 用动态阈值过滤候选
    candidates
        .into_iter()
        .filter(|&(_, _, min_sim)| min_sim >= threshold)
        .map(|(i, j, _)| {
            let start_byte = line_byte_offset(content, i);
            let end_byte = line_byte_offset(content, j) + content_lines[j].len();
            MatchSpan {
                orig_start: start_byte,
                orig_end: end_byte,
            }
        })
        .collect()
}

/// 计算第 N 行在原始字符串中的字节偏移
fn line_byte_offset(content: &str, line_idx: usize) -> usize {
    let mut offset = 0;
    for (i, line) in content.lines().enumerate() {
        if i == line_idx {
            return offset;
        }
        // 跳过行内容 + 行尾 (\n 或 \r\n)
        offset += line.len();
        let rest = &content[offset..];
        if rest.starts_with("\r\n") {
            offset += 2;
        } else if rest.starts_with('\n') {
            offset += 1;
        }
    }
    offset
}

/// 行相似度 (简单字符重叠率)
fn line_similarity(a: &str, b: &str) -> f64 {
    // 两个空字符串视为无相似度（特殊情况）
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    if a == b {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }

    let a_counts = char_counts(a);
    similarity_against_counts(b, b.chars().count(), &a_counts)
}

fn char_counts(s: &str) -> HashMap<char, usize> {
    let mut counts: HashMap<char, usize> = HashMap::new();
    for ch in s.chars() {
        *counts.entry(ch).or_insert(0) += 1;
    }
    counts
}

/// candidate 行与预计算的 expected 字符多重集的重叠系数。
/// 供滑动窗口批量比较复用：needle 侧计数只构建一次，
/// 每次比较仅为 candidate 建一份计数（原实现每比较一次分配 Vec<char>×2 + HashMap）。
fn similarity_against_counts(
    candidate: &str,
    candidate_len: usize,
    expected: &HashMap<char, usize>,
) -> f64 {
    let candidate_counts = char_counts(candidate);
    let expected_len: usize = expected.values().sum();
    let max_len = candidate_len.max(expected_len);
    if max_len == 0 {
        return 0.0;
    }
    let common: usize = candidate_counts
        .iter()
        .map(|(ch, c)| (*c).min(expected.get(ch).copied().unwrap_or(0)))
        .sum();
    common as f64 / max_len as f64
}

/// Find the line window most similar to `needle` and render it with surrounding context.
pub fn closest_line_context(content: &str, needle: &str, radius: usize) -> Option<String> {
    let content_lines: Vec<&str> = content.lines().collect();
    let needle_lines: Vec<&str> = needle.lines().collect();
    if content_lines.is_empty() || needle_lines.is_empty() {
        return None;
    }

    let window_size = needle_lines.len().min(content_lines.len());
    // needle 侧逐行字符计数只构建一次，避免 O(content×needle) 重复分配
    let needle_profiles: Vec<HashMap<char, usize>> = needle_lines
        .iter()
        .map(|line| char_counts(line.trim()))
        .collect();
    let mut best_start = 0usize;
    let mut best_score = f64::NEG_INFINITY;
    for start in 0..=content_lines.len() - window_size {
        let score = content_lines[start..start + window_size]
            .iter()
            .zip(needle_profiles.iter())
            .map(|(candidate, expected_counts)| {
                let candidate = candidate.trim();
                similarity_against_counts(candidate, candidate.chars().count(), expected_counts)
            })
            .sum::<f64>()
            / window_size as f64;
        if score > best_score {
            best_score = score;
            best_start = start;
        }
    }

    let context_start = best_start.saturating_sub(radius);
    let context_end = (best_start + window_size + radius).min(content_lines.len());
    Some(
        content_lines[context_start..context_end]
            .iter()
            .enumerate()
            .map(|(index, line)| format!("{}: {}", context_start + index + 1, line))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

// ── 策略 9: 上下文感知 ──

fn strategy_context_aware(content: &str, old_string: &str) -> Vec<MatchSpan> {
    let content_lines: Vec<&str> = content.lines().collect();
    let old_lines: Vec<&str> = old_string.lines().collect();

    if old_lines.is_empty() || old_lines.len() > content_lines.len() {
        return Vec::new();
    }

    let mut spans = Vec::new();
    let window_size = old_lines.len();
    // needle 侧逐行字符计数只构建一次，避免 O(content×needle) 重复分配
    let old_profiles: Vec<HashMap<char, usize>> = old_lines
        .iter()
        .map(|line| char_counts(line.trim()))
        .collect();

    for start in 0..=content_lines.len().saturating_sub(window_size) {
        let window = &content_lines[start..start + window_size];

        // 逐行相似度 ≥ 80%, 整体 ≥ 50%；任一行 <0.8 即可提前否决该窗口
        let mut min_line_sim = 1.0f64;
        let mut sum_sim = 0.0f64;
        for (cl, expected_counts) in window.iter().zip(old_profiles.iter()) {
            let candidate = cl.trim();
            let sim = similarity_against_counts(
                candidate,
                candidate.chars().count(),
                expected_counts,
            );
            if sim < 0.8 {
                min_line_sim = sim;
                break;
            }
            min_line_sim = min_line_sim.min(sim);
            sum_sim += sim;
        }

        let avg_sim = sum_sim / window_size as f64;

        if min_line_sim >= 0.8 && avg_sim >= 0.5 {
            let start_byte = line_byte_offset(content, start);
            let end_byte = line_byte_offset(content, start + window_size - 1)
                + content_lines[start + window_size - 1].len();
            spans.push(MatchSpan {
                orig_start: start_byte,
                orig_end: end_byte,
            });
        }
    }

    // 过滤重叠匹配: 保留每个重叠组中的第一个匹配
    // context_aware 使用滑动窗口, 可能产生重叠的 MatchSpan (如行 0-2 和行 1-3)
    // apply_replacements 要求非重叠且有序的匹配, 重叠会导致输出损坏
    deduplicate_overlapping_spans(spans)
}

// ── 替换应用 ──

/// 过滤重叠的 MatchSpan: 保留每个重叠组中的第一个匹配
///
/// apply_replacements 要求匹配按位置有序且互不重叠,
/// 重叠的 MatchSpan 会导致字节偏移错位和输出损坏。
fn deduplicate_overlapping_spans(mut spans: Vec<MatchSpan>) -> Vec<MatchSpan> {
    if spans.len() <= 1 {
        return spans;
    }
    spans.sort_by_key(|s| s.orig_start);
    let mut result = Vec::with_capacity(spans.len());
    let mut last_end = 0usize;
    for span in spans {
        if span.orig_start >= last_end {
            result.push(span);
            last_end = span.orig_end;
        }
        // 否则: 与前一个匹配重叠, 跳过
    }
    result
}

fn apply_replacements(content: &str, matches: &[MatchSpan], new_string: &str) -> String {
    if matches.is_empty() {
        return content.to_string();
    }

    let mut result = String::with_capacity(content.len() + new_string.len() * matches.len());
    let mut last_end = 0;

    for span in matches {
        result.push_str(&content[last_end..span.orig_start]);
        result.push_str(new_string);
        last_end = span.orig_end;
    }

    result.push_str(&content[last_end..]);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exact_match() {
        let content = "hello world foo bar";
        let (new, count, strategy) =
            fuzzy_find_and_replace(content, "world", "universe", false).unwrap();
        assert_eq!(new, "hello universe foo bar");
        assert_eq!(count, 1);
        assert_eq!(strategy, "exact");
    }

    #[test]
    fn test_exact_multiple_match_replace_all() {
        let content = "aaa bbb aaa ccc aaa";
        let (new, count, _strategy) = fuzzy_find_and_replace(content, "aaa", "xxx", true).unwrap();
        assert_eq!(new, "xxx bbb xxx ccc xxx");
        assert_eq!(count, 3);
    }

    #[test]
    fn test_exact_multiple_match_no_replace_all() {
        let result = fuzzy_find_and_replace("aaa bbb aaa", "aaa", "xxx", false);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, MatchError::MultipleMatches { .. }));
    }

    #[test]
    fn test_empty_old_string() {
        let result = fuzzy_find_and_replace("content", "", "new", false);
        assert!(matches!(result, Err(MatchError::EmptyOldString)));
    }

    #[test]
    fn test_identical_strings() {
        let result = fuzzy_find_and_replace("content", "same", "same", false);
        assert!(matches!(result, Err(MatchError::IdenticalStrings)));
    }

    #[test]
    fn test_no_match() {
        let result = fuzzy_find_and_replace("hello world", "not found", "new", false);
        assert!(matches!(result, Err(MatchError::NoMatch { .. })));
    }

    #[test]
    fn test_line_trimmed_match() {
        let content = "  hello world  \n  foo bar  ";
        let old = "hello world\nfoo bar";
        let (new, count, strategy) =
            fuzzy_find_and_replace(content, old, "replaced", false).unwrap();
        assert_eq!(count, 1);
        assert!(new.contains("replaced"));
        assert_eq!(strategy, "line_trimmed");
    }

    #[test]
    fn test_whitespace_normalized_match() {
        let content = "hello   world\t\tfoo";
        let old = "hello world foo";
        let (_new, count, strategy) =
            fuzzy_find_and_replace(content, old, "replaced", false).unwrap();
        assert_eq!(count, 1);
        assert_eq!(strategy, "whitespace_norm");
    }

    #[test]
    fn test_indentation_flexible_match() {
        let content = "    if x > 0:\n        print(x)\n    else:\n        print(0)";
        let old = "if x > 0:\n    print(x)\nelse:\n    print(0)";
        let (new, count, strategy) =
            fuzzy_find_and_replace(content, old, "replaced", false).unwrap();
        assert_eq!(count, 1);
        // line_trimmed 和 indent_flex 都能匹配此模式, line_trimmed 优先级更高
        assert!(
            strategy == "indent_flex" || strategy == "line_trimmed",
            "expected indent_flex or line_trimmed, got {}",
            strategy
        );
        // 关键测试: 替换发生在原始文本的正确位置
        assert_eq!(new, "replaced");
    }

    #[test]
    fn test_indentation_flexible_position_mapping_bug() {
        // 这是文档中描述的关键 BUG 的测试案例
        // 原始文本有缩进, old_string 无缩进
        // 修复前: 替换发生在错误位置 (偏移错误)
        // 修复后: 替换发生在正确位置 (行号映射)
        let content = "    if x > 0:\n        print(x)\n    else:\n        print(0)";
        let old = "if x > 0:\n    print(x)\nelse:\n    print(0)";
        let (new, _count, strategy) = fuzzy_find_and_replace(content, old, "pass", false).unwrap();
        // line_trimmed 和 indent_flex 都能匹配此模式, line_trimmed 优先级更高
        assert!(
            strategy == "indent_flex" || strategy == "line_trimmed",
            "expected indent_flex or line_trimmed, got {}",
            strategy
        );
        // 替换应该替换整个缩进块, 而不是在错误位置插入
        assert_eq!(new, "pass");
    }

    #[test]
    fn test_escape_normalized_match() {
        let content = "line1\nline2\nline3";
        let old = "line1\\nline2\\nline3";
        let (_new, count, strategy) =
            fuzzy_find_and_replace(content, old, "replaced", false).unwrap();
        assert_eq!(count, 1);
        assert_eq!(strategy, "escape_norm");
    }

    #[test]
    fn test_unicode_normalized_match() {
        let content = "He said \u{201c}hello\u{201d} and left\u{2014}gone.";
        let old = "He said \"hello\" and left--gone.";
        let (_new, count, strategy) =
            fuzzy_find_and_replace(content, old, "replaced", false).unwrap();
        assert_eq!(count, 1);
        assert_eq!(strategy, "unicode_norm");
    }

    #[test]
    fn test_normalize_unicode() {
        let input = "\u{201c}quote\u{201d} \u{2018}single\u{2019} \u{2014}dash\u{2013} \u{2026}";
        let normalized = normalize_unicode(input);
        assert_eq!(normalized, "\"quote\" 'single' --dash- ...");
    }

    #[test]
    fn test_trimmed_boundary_match() {
        let content = "  first line  \nmiddle content\n  last line  ";
        let old = "first line\nmiddle content\nlast line";
        let (_new, count, _strategy) =
            fuzzy_find_and_replace(content, old, "replaced", false).unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_block_anchor_match() {
        let content = "line1 exact\nline2 similar\nline3 close\nline4 exact";
        let old = "line1 exact\nline2 similar\nline3 close\nline4 exact";
        let (_new, count, _strategy) =
            fuzzy_find_and_replace(content, old, "replaced", false).unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_line_similarity() {
        assert_eq!(line_similarity("hello world", "hello world"), 1.0);
        assert_eq!(line_similarity("", ""), 0.0); // both empty → 0.0 (special case)
        assert!(line_similarity("hello world", "hello earth") > 0.5);
        assert!(line_similarity("abc", "xyz") < 0.3);
    }

    #[test]
    fn test_apply_replacements_single() {
        let content = "aaa bbb ccc";
        let matches = vec![MatchSpan {
            orig_start: 4,
            orig_end: 7,
        }];
        let result = apply_replacements(content, &matches, "xxx");
        assert_eq!(result, "aaa xxx ccc");
    }

    #[test]
    fn test_apply_replacements_multiple() {
        let content = "aaa bbb aaa ccc";
        let matches = vec![
            MatchSpan {
                orig_start: 0,
                orig_end: 3,
            },
            MatchSpan {
                orig_start: 8,
                orig_end: 11,
            },
        ];
        let result = apply_replacements(content, &matches, "xxx");
        assert_eq!(result, "xxx bbb xxx ccc");
    }

    #[test]
    fn test_deduplicate_overlapping_spans() {
        // Overlapping spans: [0,10) and [5,15) → keep only [0,10)
        let spans = vec![
            MatchSpan {
                orig_start: 0,
                orig_end: 10,
            },
            MatchSpan {
                orig_start: 5,
                orig_end: 15,
            },
            MatchSpan {
                orig_start: 20,
                orig_end: 30,
            }, // non-overlapping, keep
        ];
        let result = deduplicate_overlapping_spans(spans);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].orig_start, 0);
        assert_eq!(result[1].orig_start, 20);
    }

    #[test]
    fn test_deduplicate_overlapping_spans_empty() {
        let result = deduplicate_overlapping_spans(vec![]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_deduplicate_overlapping_spans_single() {
        let spans = vec![MatchSpan {
            orig_start: 0,
            orig_end: 5,
        }];
        let result = deduplicate_overlapping_spans(spans);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_context_aware_no_overlapping_output() {
        // Test that context_aware doesn't produce overlapping matches
        let content = "line1 alpha\nline2 beta\nline3 gamma\nline4 delta";
        let old = "line1 alpha\nline2 beta";
        let result = strategy_context_aware(content, old);
        // Check no overlapping spans
        for i in 0..result.len().saturating_sub(1) {
            assert!(
                result[i].orig_end <= result[i + 1].orig_start,
                "Overlapping spans found at index {}: [{},{}) and [{},{})",
                i,
                result[i].orig_start,
                result[i].orig_end,
                result[i + 1].orig_start,
                result[i + 1].orig_end
            );
        }
    }

    #[test]
    fn test_escape_normalized_with_newlines_in_line() {
        // Test escape normalization where \n literal appears within a single line
        // After normalization, the line count changes, but offset mapping should still work
        // content = "step1\nstep2\nother line" (where first \n is literal backslash+n, second is real newline)
        // old = "step1\nstep2" (where \n is literal backslash+n)
        // After normalize_escapes:
        //   content → "step1\nstep2\nother line" (with real newlines)
        //   old → "step1\nstep2" (with real newline)
        // Match found at beginning of normalized content.
        // Original span: "step1\nstep2" = bytes 0-11 (s,t,e,p,1,\,n,s,t,e,p,2) → orig_end = 12
        let content = "step1\\nstep2\nother line";
        let old = "step1\\nstep2";
        let result = strategy_escape_normalized(content, old);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].orig_start, 0);
        // "step1\nstep2" in original is 12 bytes (s,t,e,p,1,backslash,n,s,t,e,p,2)
        assert_eq!(result[0].orig_end, 12);
    }

    #[test]
    fn test_escape_offset_mapping_basic() {
        // Test build_escape_offset_mapping directly
        // content = "a\\nb" → bytes: a(0) \(1) n(2) b(3) = 4 bytes
        // normalized = "a\nb" → bytes: a(0) \n(1) b(2) = 3 bytes
        // mapping: [0, 1, 3, 4] → norm[0]=orig[0], norm[1]=orig[1], norm[2]=orig[3], norm[3]=orig[4]
        let content = "a\\nb";
        let norm_content = normalize_escapes(content);
        let mapping = build_escape_offset_mapping(content, &norm_content);
        assert_eq!(mapping[0], 0); // norm pos 0 → orig pos 0
        assert_eq!(mapping[1], 1); // norm pos 1 → orig pos 1 (after 'a')
        assert_eq!(mapping[2], 3); // norm pos 2 → orig pos 3 (after escape seq \n)
        assert_eq!(mapping[3], 4); // norm pos 3 → orig pos 4 (end)
    }
}
