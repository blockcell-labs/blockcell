# Tencent News

## Shared {#shared}

- 适合通过工具`exec` 执行对应的 `tencent-news-cli` 命令来 查询腾讯新闻热点、早报、晚报、AI 精选，以及提交使用反馈。
- 默认输出中文；默认按最新可用内容处理。
- 这个 skill 依赖本地 CLI 和 API Key；如果 CLI 未安装或未配置，先引导用户完成安装与配置，再继续查询。
- 绝对不能：
  - 用其他新闻源代替腾讯新闻。
  - 编造未执行命令的结果。
  - 未确认就输出标题、来源、时间或链接。

## Prompt {#prompt}

### 工具策略：
  1. 使用 `exec` 执行对应的 `tencent-news-cli` 命令。
  2. 只提取与用户目标最相关的新闻条目，按标题、来源、摘要、时间、标签、原文链接整理。
  3. 如果 `exec` 的 `exit_code` 为 `0` 且 `stdout` 非空，必须基于 `stdout` 生成最终回复，不要输出“无法获取”或空回复。
  4. 如果某个栏目失败，说明原因并继续输出其他已成功栏目。
  5. 如果整体命令失败，直接说明无法获取，不要切换到其他新闻源。

### 按主题执行命令

1、AI 精选内容，默认查询 AI 今日最新动态
执行 tencent-news-cli ai-daily

2、查询今日早报
执行 tencent-news-cli morning

3、查询今日晚报
执行 tencent-news-cli evening

4、查询热点新闻
执行 tencent-news-cli hot

5、提交使用问题反馈，可以把跟问题相关的上下文一并提交
执行 tencent-news-cli feedback

6、查询支持的命令及使用方式
执行 tencent-news-cli help



### 复用历史结果：
  - 用户说“刚才那个”“第 N 条”“继续”时，先复用最近一次已经拿到的列表或详情，不要重复执行命令。
  - 只有用户明确要求“刷新”“更新”时才重新执行。
### 默认值：
  - 未指定数量时，每个栏目只保留少量高价值条目，避免冗长。
  - 未指定时间范围时，默认展示最新可用内容。
### 输出格式：
  - 用 `1. **标题**` 的形式编号。
  - 每条下面按需给出 `来源`、`摘要`、`时间`、`标签`、`[查看原文](URL)`。
  - 多条新闻之间空一行。
  - 最后统一补一行 `**来源：腾讯新闻**`。
### 如果用户只要求“看看今天有什么”，优先给热点榜；如果用户明确要晨会/晚间速览，再给早报或晚报。


### 安装与配置：
  - 如果 CLI 未安装，先引导用户完成安装，再继续查询。
  - 请根据你的电脑系统，在终端执行以下其中一条命令：

    - Mac / Linux：

      ```bash
      curl -fsSL https://mat1.gtimg.com/qqcdn/qqnews/cli/hub/tencent-news/setup.sh | sh
      ```

    - Windows：

      ```powershell
      irm https://mat1.gtimg.com/qqcdn/qqnews/cli/hub/tencent-news/setup.ps1 | iex
      ```

    - 使用 NPM 安装：

      ```bash
      npm i @tencentnews/cli@latest -g
      ```

  - API Key 申请地址：<https://news.qq.com/exchange?scene=appkey>
  - 安装完成后，点击提示链接进入页面生成 API Key，然后在终端执行：

    ```bash
    tencent-news-cli apikey-set <value>
    ```

  - 特别提示：把 `<value>` 替换为你申请到的 API Key。