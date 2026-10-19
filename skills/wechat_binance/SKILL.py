import json
import os
import sys
import subprocess
import time
import requests
import argparse


# Set up logging to stderr for debugging
def log(msg):
    print(json.dumps({"log": msg}, ensure_ascii=False), file=sys.stderr)


def run_applescript(script):
    """Execute AppleScript code."""
    try:
        result = subprocess.run(
            ["osascript", "-e", script], capture_output=True, text=True, check=True, timeout=30
        )
        return True, result.stdout.strip()
    except subprocess.CalledProcessError as e:
        return False, e.stderr.strip()
    except subprocess.TimeoutExpired:
        return False, "微信自动化操作超时"


def escape_applescript(value):
    escaped = value.replace("\\", "\\\\").replace('"', '\\"')
    # AppleScript string literals cannot contain raw newlines. Join lines with
    # the `linefeed` constant so the surrounding quotes stay balanced.
    normalized = escaped.replace("\r\n", "\n").replace("\r", "\n")
    return '" & linefeed & "'.join(normalized.split("\n"))


def build_wechat_script(contact, message):
    safe_contact = escape_applescript(contact)
    safe_message = escape_applescript(message)
    return f'''
    tell application "WeChat"
        if not running then
            run
            delay 2
        end if
        activate
        reopen
    end tell

    delay 1

    tell application "System Events"
        repeat with i from 1 to 20
            if exists process "WeChat" then exit repeat
            delay 0.5
        end repeat

        tell process "WeChat"
            set frontmost to true
            repeat with i from 1 to 20
                if exists window 1 then exit repeat
                tell application "WeChat" to reopen
                delay 0.5
            end repeat
            if not (exists window 1) then error "无法找到微信窗口"

            keystroke "f" using {{command down}}
            delay 0.5
            set the clipboard to "{safe_contact}"
            keystroke "v" using {{command down}}
            delay 1

            set exactMatches to {{}}
            repeat with uiItem in entire contents of window 1
                try
                    if role of uiItem is "AXStaticText" and value of uiItem is "{safe_contact}" then
                        set end of exactMatches to uiItem
                    end if
                end try
            end repeat
            if (count of exactMatches) is not 1 then error "联系人校验失败：没有唯一的精确匹配"
            set selectedConversation to value of item 1 of exactMatches
            if selectedConversation is not "{safe_contact}" then error "联系人校验失败：搜索结果不匹配"
            click item 1 of exactMatches
            delay 1

            set winPos to position of window 1
            set winSize to size of window 1
            set clickX to (item 1 of winPos) + (item 1 of winSize) - 150
            set clickY to (item 2 of winPos) + (item 2 of winSize) - 50
            click at {{clickX, clickY}}
            delay 0.5

            set the clipboard to "{safe_message}"
            keystroke "v" using {{command down}}
            delay 0.5
            key code 36
        end tell
    end tell
    '''


def send_wechat_message(contact, message):
    """Send a message via WeChat desktop app using AppleScript."""
    return run_applescript(build_wechat_script(contact, message))


def validate_top(value):
    top = int(value)
    if not 1 <= top <= 100:
        raise ValueError("top 必须在 1 到 100 之间")
    return top


def parse_invocation(argv, stdin_input, context_input):
    raw_input = stdin_input.strip()
    for arg in argv:
        candidate = arg.strip()
        if candidate.startswith("{") and candidate.endswith("}"):
            raw_input = candidate
            break

    data = json.loads(raw_input) if raw_input else {}
    context = json.loads(context_input or "{}")
    contact = str(data.get("contact") or context.get("contact") or "").strip()
    top = validate_top(data.get("top", context.get("top", 10)))
    return contact, top


def get_binance_top_n(top=10):
    """Fetch top N coins by market cap from Binance BAPI."""
    try:
        url = "https://www.binance.com/bapi/asset/v2/public/asset-service/product/get-products"
        headers = {
            "User-Agent": "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/91.0.4472.114 Safari/537.36"
        }
        response = requests.get(url, headers=headers, timeout=10)
        if response.status_code != 200:
            return None, f"Binance API error: {response.status_code}"

        data = response.json().get("data", [])

        # Filter for USDT pairs and calculate market cap proxy (price * circulating supply)
        products = []
        for item in data:
            if item["s"].endswith("USDT") and item.get("cs") and item.get("c"):
                try:
                    price = float(item["c"])
                    supply = float(item["cs"])
                    market_cap = price * supply
                    products.append(
                        {
                            "symbol": item["s"].replace("USDT", ""),
                            "price": price,
                            "market_cap": market_cap,
                            "change": item.get(
                                "r", "0"
                            ),  # 'r' is 24h price change ratio
                        }
                    )
                except (ValueError, TypeError):
                    continue

        # Sort by market cap descending
        products.sort(key=lambda x: x["market_cap"], reverse=True)
        return products[:top], None
    except Exception as e:
        return None, str(e)


def main():
    log(f"sys.argv: {sys.argv}")

    stdin_input = sys.stdin.read()
    contact, top = parse_invocation(
        sys.argv[1:], stdin_input, os.environ.get("BLOCKCELL_SKILL_CONTEXT", "{}")
    )

    log(f"Final resolved params: contact={contact}, top={top}")

    if not contact:
        log("No contact provided, defaulting to '文件传输助手'")
        contact = "文件传输助手"

    ## ------------------------
    # Fetch Binance Data
    # log(f"Fetching Binance data...")
    top_n, error = get_binance_top_n(top)

    if error:
        result = {"display_text": f"获取币安数据失败: {error}"}
        print(json.dumps(result, ensure_ascii=False))
        sys.exit(1)

    # Format message
    msg_lines = [f"📊 币安市值 Top {top} 行情", ""]
    for i, coin in enumerate(top_n, 1):
        # change_val = float(coin["change"]) * 100
        # change_str = f"+{change_val:.2f}%" if change_val >= 0 else f"{change_val:.2f}%"

        msg_lines.append(f"{i}. {coin['symbol']}: ${coin['price']:,} ")

    msg_lines.append(f"\n⏰ 更新时间: {time.strftime('%Y-%m-%d %H:%M:%S')}")
    message = "\n".join(msg_lines)

    log(f"Sending to WeChat contact: {contact}")
    success, output = send_wechat_message(contact, message)

    if success:
        result = {
            "display_text": f"已成功发送币安 Top {top} 行情至微信联系人: {contact}\n\n{message}",
            "summary_data": {
                "contact": contact,
                "coins": [c["symbol"] for c in top_n],
            },
        }
    else:
        result = {"display_text": f"微信发送失败: {output}", "error": output}

    print(json.dumps(result, ensure_ascii=False))


if __name__ == "__main__":
    try:
        main()
    except Exception as e:
        log(f"Fatal error: {str(e)}")
        print(json.dumps({"error": str(e)}, ensure_ascii=False), file=sys.stderr)
        sys.exit(1)
