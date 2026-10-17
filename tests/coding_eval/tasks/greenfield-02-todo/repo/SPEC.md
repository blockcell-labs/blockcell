实现 `todo.py` 中的 `TodoStore(path)`，使用 JSON 文件持久化：
- `add(title: str) -> dict`：新增待办项并持久化，返回包含 `"id"`、`"title"`、`"completed"`（初始为 `False`）的字典；
- `complete(item_id)`：将对应 `id` 的待办项标记为 `completed = True` 并持久化；
- `list() -> list[dict]`：返回所有待办项字典列表（每项含 `"id"`、`"title"`、`"completed"`）。
补充 `README.md`。
