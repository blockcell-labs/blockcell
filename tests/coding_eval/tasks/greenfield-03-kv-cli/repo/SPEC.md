实现 `kv.py` 的 `main(argv: list[str])`，以顶层 JSON 对象文件（`argv[0]` 为文件路径）持久化键值：
- `main([path, "set", key, value])`：写入键值并持久化到文件，返回整数状态码 `0`；
- `main([path, "get", key])`：读取并返回对应键的字符串值（`str`）；
- `main([path, "delete", key])`：从 JSON 对象中删除该键并持久化，返回整数状态码 `0`。
补充 `README.md`。
