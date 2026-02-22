# Article 07: Browser Automation — Let AI Control the Web for You

> Series: *In-Depth Analysis of the Open Source Project “blockcell”* — 7/14

---

## Why browser automation?

A lot of information can only be obtained through a browser:
- Websites that require login
- Dynamic content (JavaScript-rendered)
- Data that only appears after clicking or interacting

A normal `web_fetch` tool can only retrieve static HTML, so it cannot handle these cases.

blockcell’s `browse` tool solves this by controlling a real browser via **CDP (Chrome DevTools Protocol)** — Chrome/Edge/Firefox — directly.

---

## Two browser-related tools

blockcell provides two browser tools with different goals:

| Tool | Protocol | Mode | Typical usage |
|------|----------|------|---------------|
| `browse` | CDP WebSocket | headless/headful | automation, data extraction |
| `chrome_control` | AppleScript | headful (visible) | visual operation, demos |

This article focuses on `browse`, which is the more powerful option.

---

## What is CDP?

CDP (Chrome DevTools Protocol) is a debugging interface exposed by Chromium-based browsers.

The DevTools you open with F12 in Chrome is built on top of CDP.

By connecting to the CDP endpoint via WebSocket, blockcell can:
- Navigate pages
- Read DOM structure
- Simulate mouse clicks and keyboard input
- Take screenshots
- Execute JavaScript
- Manage cookies
- Intercept network requests

---

## Key feature: Accessibility Tree

This is one of the smartest design choices in blockcell’s browser tooling.

Traditional browser automation (like Selenium) locates elements using CSS selectors or XPath, for example:

```
document.querySelector("#submit-button")
```

The problem: selectors break easily when the page changes.

blockcell uses the **Accessibility Tree** to describe page structure semantically:

```
The snapshot action returns:
[1] RootWebArea "GitHub" focused
  [2] navigation "Global"
    [3] link "Homepage" url=https://github.com
  [4] main ""
    [5] heading "Let's build from here" level=1
    [6] textbox "Search or jump to..." focused
    [7] button "Sign in" @e7
    [8] button "Sign up" @e8
```

Each element may have a reference like `@e7`. The AI can interact using the reference directly:

```json
{"action": "click", "ref": "@e7"}
```

This is more stable than CSS selectors because the accessibility tree reflects the semantic structure, not the exact HTML implementation.

---

## 35+ actions

`browse` supports 35+ actions covering nearly every aspect of browser automation:

### Navigation
```
navigate    - open a URL
back        - go back
forward     - go forward
reload      - reload
get_url     - get current URL
```

### Reading content
```
snapshot    - get accessibility tree (recommended)
get_content - get page content (converted to Markdown)
execute_js  - run JavaScript
```

### Interaction
```
click       - click an element
fill        - fill a form field
type_text   - simulate keyboard typing
press_key   - press keys (Enter/Tab/Escape, etc.)
scroll      - scroll the page
wait        - wait (ms)
```

### Screenshots
```
screenshot  - screenshot (PNG)
pdf         - generate PDF
```

### Tab management
```
tab_list    - list tabs
tab_new     - open a new tab
tab_close   - close a tab
tab_switch  - switch tabs
```

### Cookie management
```
cookies_get   - get cookies
cookies_set   - set cookies
cookies_clear - clear cookies
```

### Advanced features
```
upload_file       - upload files
dialog_handle     - handle dialogs (alert/confirm/prompt)
network_intercept - intercept network requests
network_block     - block specific URL patterns
set_headers       - set request headers
set_viewport      - set viewport size
list_browsers     - list available browsers
```

---

## Examples

### Example 1: login and fetch data

```
You: Log into GitHub and show my notifications
```

A typical execution flow:

```
1. browse navigate "https://github.com/login"
2. browse snapshot → find username textbox @e5
3. browse fill ref=@e5 value="your username"
4. browse fill ref=@e6 value="your password"
5. browse click ref=@e7  (sign-in button)
6. browse wait 2000
7. browse navigate "https://github.com/notifications"
8. browse get_content → return notifications in Markdown
```

### Example 2: auto-fill a form

```
You: Fill out the contact form:
    name: Zhang San, email: zhangsan@example.com,
    message: I’d like to learn more about your product
```

```
1. browse snapshot → analyze form structure
2. browse fill ref=@e3 value="Zhang San"
3. browse fill ref=@e4 value="zhangsan@example.com"
4. browse fill ref=@e5 value="I’d like to learn more about your product"
5. browse click ref=@e6  (submit)
6. browse snapshot → confirm submission succeeded
```

### Example 3: screenshot and analyze

```
You: Take a screenshot of blockcell’s website,
    then tell me what the main sections are
```

```
1. browse navigate "https://blockcell.dev"
2. browse screenshot → save as screenshot.png
3. image_understand analyze path=screenshot.png
   → “The page contains: nav bar, hero section, feature list...”
```

### Example 4: monitor price changes

```
You: Monitor the price of a product on JD.com.
    If it drops below 500 RMB, take a screenshot and notify me
```

The AI can create a scheduled job:

```
Every hour:
1. browse navigate "product URL"
2. browse snapshot → locate the price element
3. Extract the numeric price
4. If < 500:
   browse screenshot
   notification send "Price dropped to {price}!"
```

---

## Multi-browser support

blockcell supports three browser engines:

```json
{
  "action": "navigate",
  "url": "https://example.com",
  "browser": "chrome"   // or "edge" / "firefox"
}
```

The `list_browsers` action automatically detects which browsers are installed:

```
You: Which browsers are available on this machine?
AI: browse list_browsers
    Available:
    - chrome: /Applications/Google Chrome.app (recommended)
    - edge: /Applications/Microsoft Edge.app
```

---

## Session management

By default, each `browse` call reuses existing browser sessions.

That means:
- login state persists
- cookies are preserved
- you don’t need to re-open a browser each time

Example:

```bash
blockcell agent
You: List active browser sessions
AI: browse session_list
    Active sessions:
    - session_1: chrome, 3 tabs
```

---

## Network interception (advanced)

The `network_intercept` action can pause and modify matching requests. Useful for:

- Injecting custom headers (e.g., Authorization)
- Mocking specific API responses
- Blocking ads or tracking scripts

```json
{
  "action": "network_intercept",
  "url_pattern": "*/api/v1/*"
}
```

After interception is enabled, each matching request pauses and the AI can decide to continue (`network_continue`) or fulfill it with a custom response.

---

## `chrome_control` vs `browse`: how to choose

**Use `browse` (CDP) when:**
- you need headless mode (run in background)
- you need precise element interactions
- you need screenshots and PDF generation
- you need network interception
- you’re doing automation testing

**Use `chrome_control` (AppleScript) when:**
- you want to watch the operation visually
- you are presenting/demonstrating
- you need to interact with Chrome extensions
- you only need simple click/type operations

---

## Notes

1. **A browser must be installed**: `browse` requires Chrome/Edge/Firefox
2. **Headless by default**: runs headlessly and won’t show a window
3. **Anti-bot measures**: some sites require User-Agent tweaks or cookies
4. **Performance**: browser automation is slower than raw HTTP and best for JS-rendered pages

---

## Summary

blockcell’s browser automation provides:

- **CDP-based control**: drive real browsers and handle JS rendering
- **Accessibility tree**: semantic element targeting, more stable than CSS selectors
- **35+ actions**: navigation, interaction, screenshots, tabs, network interception
- **Multi-browser**: Chrome/Edge/Firefox supported
- **Session reuse**: persistent login/cookies

Next, we’ll cover Gateway mode — how to turn blockcell into an API service.

---

*Previous: [Multi-channel access — Telegram/Slack/Discord/Feishu all supported](./06_channels.md)*
*Next: [Gateway mode — turning AI into a service](./08_gateway_mode.md)*

*Repo: https://github.com/blockcell-labs/blockcell*
*Website: https://blockcell.dev*
