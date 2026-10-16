# tools 内容生产集群 Review 报告（Wave 2 · 机制层）

> 只读审查 · 2026-10-16 · 子代理 w2-content

## 规模

| 项 | 值 |
|---|---|
| 审查文件 | 9 个（office_write.rs, office.rs, data_process.rs, chart_generate.rs, audio_transcribe.rs, tts.rs, ocr.rs, image_understand.rs, video_process.rs） |
| 总 LOC | ~6,400（其中测试约 1,300） |
| 外部依赖 | python3（python-pptx/openpyxl）、ffmpeg/ffprobe、tesseract、whisper/whisper.cpp、edge-tts、piper、macOS say、OpenAI/Anthropic/Gemini API、docx-rs、calamine、zip、quick-xml、reqwest |
| 注册位置 | crates/tools/src/registry.rs:206-233（8 个 Tool 全部注册；office.rs 为被读取侧复用的函数库，无独立 Tool） |

## 模块地图（文件职责表）

| 文件 | LOC | 职责 |
|---|---|---|
| office_write.rs | 1000 | office_write 工具：docx 由 docx-rs 原生生成（含 numbering/styles/表格/图片/作者注入防护）；pptx/xlsx 生成 Python 脚本落盘 `workspace/tmp/_office_gen.py` 后以 python3 子进程执行 |
| office.rs | 264 | Office 读取库：read_excel（calamine→markdown 表）、read_docx/read_pptx（zip + quick-xml 事件流抽 `w:t`/`a:t`），被 fs 类读取链路复用 |
| data_process.rs | 1207 | data_process 工具：CSV 读写（csv crate）、query（过滤/排序/投影）、stats（分组聚合/分位数/相关系数）、transform（6 种 op），CPU 密集操作走 spawn_blocking |
| chart_generate.rs | 762 | chart_generate 工具：matplotlib/plotly 脚本生成 + python3 子进程；支持 `custom_script` 自定义 Python |
| audio_transcribe.rs | 728 | audio_transcribe 工具：whisper CLI / whisper.cpp / OpenAI Whisper API 三后端，ffmpeg 提取/转码音频 |
| tts.rs | 579 | tts 工具：macOS say / piper / edge-tts / OpenAI TTS 四后端，auto 模式依序降级 |
| ocr.rs | 480 | ocr 工具：tesseract / macOS Vision（python3+PyObjC）/ OpenAI Vision 三后端 |
| image_understand.rs | 495 | image_understand 工具：图片 base64 后送 OpenAI/Anthropic/Gemini 视觉 API，4 种 action |
| video_process.rs | 891 | video_process 工具：ffmpeg 封装 10 个 action（clip/merge/subtitle/thumbnail/convert/extract_audio/resize/compress/watermark/info），含 ffprobe 元数据与 libass 滤镜预检 |

## 问题清单

| 级别 | 位置 文件:行 | 问题 | 证据/说明 |
|---|---|---|---|
| P0 | ocr.rs:218-281（`ocr_vision`） | Python 代码注入：用户可控 `language` 未转义直接插入 Python 源码 | `lang_array` 构造：`other => other.to_string()` 后 `format!("'{}'", l)` 无任何引号转义；同函数内 `path` 做了 `.replace('\'', "\\'")` 而 language 没有。构造 `language="zh',__import__('os').system('id'),'"` 即可闭合字符串执行任意命令（macOS 路径，`backend=auto` 且无 tesseract 时默认走 vision） |
| P1 | video_process.rs:44-47 vs crates/agent/src/runtime/path_security.rs:79-85 | video_process 写入完全绕过 path write 策略 | schema 字段名为 `output`，而 runtime `extract_path_accesses` 提取的是 `output_path`（永不存在于参数中）→ deny/confirm 规则、默认 confirm 策略对该工具所有输出均不生效。docs/22 明确把 video_process 列入 `write` ops 工具清单，属实现与契约双向背离 |
| P1 | office_write.rs:754-758；chart_generate.rs:256-260 | 固定路径临时脚本 + symlink 跟随 + 并发 clobber | `workspace/tmp/_office_gen.py`（pptx/xlsx 共用同一文件名）与 `tmp/_chart_gen.py` 为固定名：并发调用互相覆写（TOCTOU）；`std::fs::write` 跟随符号链接，工作区内预置 symlink 可以 agent 身份任意覆写文件（CWE-59/CWE-367）。audio_transcribe.rs:663（`{stem}_tmp.wav`）、video_process.rs:437（`media/_concat_list.txt`）为同模式轻量版 |
| P1 | office.rs:80-92, 102-132 | zip 解压炸弹：docx/pptx 读取无任何大小/entry 数量上限 | `read_to_string` 直接将 ZIP entry 全量解压进内存，高压缩比恶意文档可 OOM；crate 内已有 `bounded_io` 模块（用于 HTTP）但未用于 zip。calamine `open_workbook_auto` 同样全量载入 xlsx |
| P1 | data_process.rs:230（`read_csv_to_json`）；audio_transcribe.rs:484（`try_api_transcribe`） | 大文件全量入内存，无流式/上限 | CSV 先 `fs::read_to_string` 再全量转 `Vec<Value>`，`limit` 在解析后才 truncate；Whisper API 路径 `fs::read` 整个音视频文件且无 OpenAI 25MB 上限预检，GB 级输入即 OOM（`limit` 语义形同虚设） |
| P1 | video_process.rs:266-267（`run_ffmpeg`） | 字节切片 panic：`&stderr[stderr.len()-1000..]` | stderr 为 UTF-8 String，但 1000 字节回退可能落在多字节字符中间 → "byte index is not a char boundary" panic，进程崩溃而非返回错误（ffmpeg 中文/emoji stderr 场景可稳定触发） |
| P2 | chart_generate.rs:179-188 | output_path 不做 workspace 展开，与其余工具行为不一致 | 直接 `op.to_string()` 使用；相对路径落到**进程 CWD** 而非 `ctx.workspace`，其余 8 个工具均有 expand/resolve 逻辑，文件会写到意外位置且 path policy 判定与实际落盘路径不一致 |
| P2 | audio_transcribe.rs:641-681 | 临时 wav 竞态 + 错误路径泄漏 | `tmp/{stem}_tmp.wav` 确定性命名，同 stem 并发转换互相覆写；`try_whisper_cpp` 内任何 `?` 提前返回都跳过 450-452 行的清理，失败即留垃圾 |
| P2 | video_process.rs:437-465 | merge concat list 固定文件名 | `media/_concat_list.txt` 并发 merge 交叉写入行内容，产出错误合并结果；concat 内容也未对路径中的 `'` 转义 |
| P2 | video_process.rs:586-590 | `extra_args` 任意 ffmpeg 参数注入 | 按空白切分原样追加：可插入额外 `-i`/输出文件/协议（如读取任意本地文件写入 output），相当于一个无确认的次级命令执行通道；该工具未实现 `requires_confirmation` |
| P2 | image_understand.rs:196-216 | 图片内存放大 | 每张图同时驻留 `bytes` + base64 String + data_url（约 3×文件大小），compare 多图再 ×N，无尺寸/大小/数量上限；`reqwest::Client::new()` 每次新建，无连接复用 |
| P2 | data_process.rs:394-421（`write_json_to_csv`） | CSV 公式注入 | 写入不对 `=`/`+`/`-`/`@` 开头单元格做消毒，导出文件用 Excel/WPS 打开可触发公式执行（数据源常来自外部，属经典 CSV injection） |
| P2 | 全部 9 文件 | 子进程/HTTP 均无超时 | python3（office_write:766、chart_generate:265、ocr.rs:283）、ffmpeg/ffprobe/whisper/edge-tts、piper、reqwest（tts.rs:472、ocr.rs:328 等）一律 `Command::output()`/裸 send 无限等待；恶意或超大输入可无限占用 worker |
| P2 | office_write/chart_generate/audio_transcribe/tts/ocr/image_understand/video_process（全部未实现 `execution_environment`） | 审计失真：实际派生外部子进程却默认声明 `InProcess` | lib.rs:569 默认值为 InProcess；`exec` 工具有正确示范（exec.rs:149-155 按 sandbox 策略声明 Shell/FullAccess）。本集群全部启动 python/ffmpeg/tesseract 等子进程，审计事件记录的环境是错的 |
| P2 | ocr.rs:303-372（`ocr_api`）；image_understand.rs:400-403 | API 凭据与端点管理不统一 | Gemini key 走 URL query（`?key=`）比 header 更易进入日志/代理记录；OpenAI/Anthropic 用 header（正确）。模型名硬编码分散 6 处（gpt-4o-mini/claude-sonnet-4-20250514/gemini-2.0-flash/tts-1/whisper-1），不可配置、有陈旧风险。凭据解析本身（config providers→env）正确且未记日志 |
| P3 | docs/03_tools_system.md:253 | 文档漂移：docx 后端 | 文档写 "后端：Python python-pptx/python-docx/openpyxl"，实际 docx 为 docx-rs 原生生成（info action 的 `native_formats` 已反映）；video_process 的 resize/watermark/info 也未在文档罗列 |
| P3 | office_write.rs:716-719；data_process.rs:13-17；tts.rs:535-541 等 8 处 | `expand_path`/`resolve_path` 8 份实现语义不一 | office_write 用 `replacen('~', home, 1)` 会把 `~x` 错展开为 `$HOME/x`；tts 对 `~` 开头路径**原样返回**（不展开），导致字面创建 `~` 目录；data_process 仅处理 `~/`。建议抽公共 helper |
| P3 | tts.rs:216, 318 | speed 未校验 | schema 声明 0.25-4.0 但 `validate` 不检查；负值经 `(175.0*speed) as u32` 回绕成巨大 wpm 传给 say |
| P3 | audio_transcribe.rs:288-290 | `--verbose False` 疑无效 | 注释称 "Verbose off"，但 whisper CLI 的 `--verbose` 若为 store_true 则额外位置参数 `False` 会被 argparse 拒绝（本地 whisper 后端可能永远失败并降级 API）；若为 `type=bool` 则 `bool("False")==True` 同样无效。需实机验证 |
| P3 | office.rs:145-194 | XML 解析细节 | 不匹配 `Event::CData`，含 CDATA 的 docx/pptx 丢文本；read_excel 生成的 markdown 表不转义单元格内 `|` 与换行，内容含管道符时表格损坏 |
| P3 | data_process.rs:566；多处 | 静默放行 + 魔法常量 | `match_filter` 未知 operator 返回 `true`（过滤条件写错不报错，结果虚胖）；相关系数阈值 0.8/0.5/0.3、`1e-15`、`*1000/10000` 舍入、5000 截断等内联无常量 |
| P3 | video_process.rs 各 action | ffmpeg 缺 `-nostdin` | 守护进程/IM 场景下 ffmpeg 可能吞掉 stdin（业界标准做法是 `-nostdin`） |

## 亮点

- **docx-rs 原生生成质量高**：标题/表格/编号/图片/分页全覆盖，作者字段改写 core.xml 时用 `quick_xml::escape::escape` 防注入（office_write.rs:472-482）；图片先 `image::load_from_memory` 校验可解码再入档，规避 docx-rs `Pic::new` panic（573-587），并有充分注释。
- **测试有真 round-trip**：office_write.rs:942-993 在内存中生成 docx 后用 zip crate 解包断言 5 个 XML part，再经 `office::read_docx` 回读校验文本（含 `<Q1>`、`Ann &amp; Bob` 转义、broken/missing 图片跳过），是全集群测试质量最高的一个。
- **Python 脚本插值防注入到位**：统一 `quote_python_str` + JSON payload 先转 `\\` 再转 `'` 的双重转义（office_write.rs:324-325），单引号逃逸攻击面已闭合（与 ocr.rs 的疏漏形成对照）。
- **子进程错误处理规范**：退出码检查、stderr 截断回传、输出文件存在性二次校验（office_write.rs:775-792）。
- **video_process 工程细节**：`ensure_ffmpeg_filter_available` 预检 libass 并给出可操作的修复指引（276-313）；subtitle 路径单引号转义；resize 强制偶数维度适配 h264（627-639）。
- **data_process CPU 工作正确走 spawn_blocking**（157-199），纯函数（percentile/correlation/filter/compare）单测边界覆盖充分。
- **凭据处理**：tts/ocr/image_understand 统一 config→env 解析且不打印 key；API 错误回显带 status code 便于排障。

## 建议

1. **立即修 P0**：`ocr_vision` 的 language 与 path 一并用同一转义函数（或干脆把参数改为 argv 传参而非字符串插值）。
2. **video_process 字段对齐**：要么 schema 改回 `output_path`，要么在 path_security.rs 提取 `output`；建议同时给单测锁定 "每个 schema 里的路径字段都必须出现在 extract_path_accesses 中"。
3. **临时文件治理**：三处固定名临时文件统一改用 `tempfile` crate 的 `NamedTempFile`/`tempdir`（O_EXCL 创建 + Drop 清理），根治 symlink 与并发问题；所有外部子进程加 `tokio::time::timeout`。
4. **读取侧加固**：office.rs 的 zip 读取加总解压上限与 entry 数上限（复用/推广 `bounded_io` 思路）；data_process/audio_transcribe 改流式（csv crate 可直接从 `File` 读；Whisper API 先做 25MB 预检并给出明确错误）。
5. **video_process `run_ffmpeg`** 用 `char_indices` 找安全边界替代裸字节切片，消除 panic。
6. **CSV 导出消毒**：`=`/`+`/`-`/`@`/Tab/CR 前缀加 `'` 前缀转义。
7. **审计补全**：本集群工具按实际行为声明 `execution_environment`（写文件→WorkspaceWrite，起子进程→Shell/FullAccess，调 API→Network）。
8. **补 fixture 测试**：目前 office 读取侧（docx/pptx/xls）只有一个 unsupported-extension 错误用例，`find` 确认无任何 fixture 文件；建议内置 3-4 个最小合法/畸形（zip bomb 缩比版、坏 XML、错编码）fixture 锁定解析行为。
9. **收敛魔法量与重复实现**：8 份路径展开合成 1 个 helper；相关系数阈值、截断长度、模型名提为常量或配置项。
