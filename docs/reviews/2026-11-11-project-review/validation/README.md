# 复核验证材料

日期 2026-11-12；基线 `main / 5ca38be861b4a3e8f7a1e8d332e242b42ed25710`。

## 已执行检查

`rust-core-storage.log`：实际运行以下命令，core 132、storage 105 项测试通过。仅使用本机已安装工具链，不修改项目 toolchain 或根 Cargo.lock。

```sh
RUSTC="$(rustup which --toolchain 1.96.0 rustc)" \
RUSTDOC="$(rustup which --toolchain 1.96.0 rustdoc)" \
rustup run 1.96.0 cargo test -p blockcell-core -p blockcell-storage --offline --locked --lib
```

`probe_api.rs` 调用真实 crate API，`run_api_probes.py` 在临时目录生成独立 Cargo 工程，使用根锁文件作为依赖版本种子，仅允许临时锁文件补充探针包。编译产物使用项目 target。重跑入口（从项目根目录）：

```sh
python3 docs/reviews/2026-11-11-project-review/validation/run_api_probes.py
```

要求已安装 Rust 1.96 及离线依赖缓存；临时人工文件放在 `/private/tmp`，不调用网络、模型或真实渠道。脚本只记录现状，不断言“缺陷必须一直存在”；退出 0 仅代表观测运行完成。

`api-probes.jsonl` 为本轮 stdout，共 11 条；`api-probes-build.log` 为编译/执行 stderr。临时路径中的文件已清理，输出中的随机消息 ID/临时目录/快照时间每次不同。

| 输出 ID | 观测 |
| --- | --- |
| B1-01 | 首次读取 B 得到 A 内容；clear(B) 后 A/B 文件均消失 |
| B1-02 | 删除 provider 后 save/load 恢复旧项 |
| B1-03 | 非法时区写入成功，启动 load 拒绝 |
| B1-04 | 删除首个 Agent 后 dev 的额度变成 ops 的旧值 |
| B1-05 | 缺失记忆 ID 返回 Err |
| NEW-model-token-setting（N-04） | ModelEntry 序列化结果没有 maxTokens |
| B3-01 | 同一人工路径 host policy=Deny，Rhai 嵌套读取仍成功；不是完整 Runtime 入口 |
| B3-02 | ZIP 经目标既有链接改写外部临时哨兵 |
| NEW-implicit-decompress-destination（N-01） | 无 destination 时在外部归档父目录创建产物；授权遗漏另由静态链确认 |
| B3-03 | list 展示的 frontmatter name 无法直接 view，物理目录名可用 |
| B3-04 | 删除后快照目录仍作为技能列出 |

真实 API 运行与静态授权链共同支撑报告，不代表所有真实入口、平台或并发情形已测。交叉复核者另外执行的脚本/store 最小实验，其方法和输出摘要保存在相应 revalidation 笔记中；此目录未保存那些实验的全部临时 harness，不宣称它们可由此单一入口重跑。
