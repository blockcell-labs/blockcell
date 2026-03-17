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
            ["osascript", "-e", script], capture_output=True, text=True, check=True
        )
        return True, result.stdout.strip()
    except subprocess.CalledProcessError as e:
        return False, e.stderr.strip()


def send_wechat_message(contact, message):
    """Send a message via WeChat desktop app using AppleScript."""
    safe_contact = contact.replace("\\", "\\\\").replace('"', '\\"')
    safe_message = message.replace("\\", "\\\\").replace('"', '\\"')

    script = f'''
    -- 1. Ensure WeChat is running
    tell application "WeChat"
        if not running then
            run
            delay 2 -- Wait for launch
        end if
        activate
        reopen -- Force main window to open if closed
    end tell

    delay 1

    tell application "System Events"
        -- Wait for process to appear
        repeat with i from 1 to 20
            if exists process "WeChat" then exit repeat
            delay 0.5
        end repeat
        
        tell process "WeChat"
            set frontmost to true
            
            -- Wait for main window to appear (up to 10 seconds)
            repeat with i from 1 to 20
                if exists window 1 then exit repeat
                
                -- If window doesn't exist, try to reopen again
                tell application "WeChat" to reopen
                delay 0.5
            end repeat
            
            if not (exists window 1) then
                error "无法找到微信窗口。可能原因：\n1. 微信未登录\n2. 缺少辅助功能权限 (Accessibility)\n请检查 System Settings -> Privacy & Security -> Accessibility 是否允许终端/Trae控制电脑。"
            end if

            try
                set value of attribute "AXMinimized" of window 1 to false
            end try
            delay 0.5
            
            -- Step 1: Search for contact
            keystroke "f" using {{command down}}
            delay 0.5
            
            do shell script "echo " & quoted form of "{safe_contact}" & " | pbcopy"
            delay 0.2
            keystroke "v" using {{command down}}
            delay 1.0
            
            key code 36
            delay 1.0
            
            -- Step 2: Focus Input Box (Click bottom-right region with randomization)
            set winPos to position of window 1
            set winSize to size of window 1
            set winX to item 1 of winPos
            set winY to item 2 of winPos
            set winW to item 1 of winSize
            set winH to item 2 of winSize
            
            -- Calculate region start (bottom-right corner - region size 300x100)
            set regionW to 300
            set regionH to 100
            set startX to winX + winW - regionW
            set startY to winY + winH - regionH
            
            -- Randomize click within region for more natural behavior
            set clickX to startX + (random number from 20 to (regionW - 20))
            set clickY to startY + (random number from 20 to (regionH - 20))
            
            -- Try to use cliclick if available for more reliable click
            try
                do shell script "/usr/local/bin/cliclick c:" & (clickX as integer) & "," & (clickY as integer)
            on error
                -- Fallback to System Events click if cliclick missing
                click at {{clickX, clickY}}
            end try
            
            delay 0.5
            
            -- Step 3: Type and Send
            do shell script "export LANG=zh_CN.UTF-8; echo " & quoted form of "{safe_message}" & " | pbcopy"
            delay 0.5
            keystroke "v" using {{command down}}
            delay 0.5
            key code 36
        end tell
    end tell
    '''
    return run_applescript(script)


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

    # Parse Input
    try:
        raw_input = sys.stdin.read().strip()
    except Exception:
        raw_input = ""

    contact = ""
    top = 10

    # The blockcell runtime passes arguments as: argv=["wechat_binance", "{\"contact\": \"发财群\", ...}"]
    # The first argument (sys.argv[0]) is the script path or command name.
    # The second argument (sys.argv[1]) is the method name ("wechat_binance").
    # The third argument (sys.argv[2]) is the JSON string payload.
    # Note: Sometimes it passes just the JSON string as argv[1].
    # Let's search all arguments for a valid JSON string.
    raw_input = ""
    for arg in sys.argv[1:]:
        if arg.startswith("{") and arg.endswith("}"):
            raw_input = arg
            break

    # Try JSON
    try:
        if raw_input:
            data = json.loads(raw_input)
            if not contact:
                contact = data.get("contact", "")
            if "top" in data:
                top = int(data.get("top", top))
    except Exception as e:
        log(f"Failed to parse JSON input: {e}")
        pass

    # Try Context from Environment
    if not contact or top == 10:
        raw_ctx = os.environ.get("BLOCKCELL_SKILL_CONTEXT", "{}")
        try:
            ctx = json.loads(raw_ctx)
            if not contact:
                contact = ctx.get("contact", "")
            if "top" in ctx:
                top = int(ctx.get("top"))
        except Exception:
            pass

    if not contact and "给" in raw_input:
        parts = raw_input.split("给", 1)
        if len(parts) > 1:
            sub = parts[1].split("发", 1)
            contact = sub[0].strip()

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
