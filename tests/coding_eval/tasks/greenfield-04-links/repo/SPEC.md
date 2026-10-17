实现 `links.py`：
- `extract_links(markdown: str) -> list[dict]`：提取非图片 Markdown 链接（忽略 `![alt](url)`），返回形如 `[{"text": "...", "url": "..."}]` 的字典列表；
- `main(argv: list[str]) -> str`：读取 `argv[0]` 指定的文件路径，提取链接并返回 JSON 序列化后的字符串（调用方可直接 `json.loads(main([path]))`）。
补充 `README.md`。
