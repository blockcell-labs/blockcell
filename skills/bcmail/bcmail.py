#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
通用邮箱 CLI 工具
支持发送邮件、查看邮件、搜索邮件等操作

环境变量:
    BCMAIL_EMAIL       邮箱地址
    BCMAIL_PASSWORD    密码或客户端授权码

用法:
    python bcmail.py send --to recipient@example.com --subject "Hello" --body "World"
    python bcmail.py send --to user@qq.com --subject "Test" --html --body "<h1>Hi</h1>"
    python bcmail.py send --to user@gmail.com --subject "Report" --attach file.pdf --body "See attachment"

    python bcmail.py inbox --limit 5
    python bcmail.py inbox --unread
    python bcmail.py read --id 123
    python bcmail.py search --keyword "测试"

    python bcmail.py unread
    python bcmail.py folders
"""

import argparse
import email
import imaplib
import os
import re
import smtplib
import sys
from email import encoders
from email.header import decode_header
from email.mime.base import MIMEBase
from email.mime.multipart import MIMEMultipart
from email.mime.text import MIMEText


SUPPORTED_PROVIDERS = {
    "163.com": {
        "imap_server": "imap.163.com",
        "imap_port": 993,
        "smtp_server": "smtp.163.com",
        "smtp_port": 465,
    },
    "vip.163.com": {
        "imap_server": "imap.vip.163.com",
        "imap_port": 993,
        "smtp_server": "smtp.vip.163.com",
        "smtp_port": 465,
    },
    "126.com": {
        "imap_server": "imap.126.com",
        "imap_port": 993,
        "smtp_server": "smtp.126.com",
        "smtp_port": 465,
    },
    "vip.126.com": {
        "imap_server": "imap.vip.126.com",
        "imap_port": 993,
        "smtp_server": "smtp.vip.126.com",
        "smtp_port": 465,
    },
    "188.com": {
        "imap_server": "imap.188.com",
        "imap_port": 993,
        "smtp_server": "smtp.188.com",
        "smtp_port": 465,
    },
    "vip.188.com": {
        "imap_server": "imap.vip.188.com",
        "imap_port": 993,
        "smtp_server": "smtp.vip.188.com",
        "smtp_port": 465,
    },
    "yeah.net": {
        "imap_server": "imap.yeah.net",
        "imap_port": 993,
        "smtp_server": "smtp.yeah.net",
        "smtp_port": 465,
    },
    "gmail.com": {
        "imap_server": "imap.gmail.com",
        "imap_port": 993,
        "smtp_server": "smtp.gmail.com",
        "smtp_port": 587,
    },
    "outlook.com": {
        "imap_server": "outlook.office365.com",
        "imap_port": 993,
        "smtp_server": "smtp.office365.com",
        "smtp_port": 587,
    },
    "qq.com": {
        "imap_server": "imap.qq.com",
        "imap_port": 993,
        "smtp_server": "smtp.qq.com",
        "smtp_port": 587,
    },
}


class BCMailCLI:
    """通用邮箱 CLI 客户端"""

    def __init__(self):
        self.email = os.getenv("BCMAIL_EMAIL")
        self.password = os.getenv("BCMAIL_PASSWORD")
        self.provider = None
        if self.email:
            self.provider = self._resolve_provider()

    def _ensure_auth(self):
        """确保已通过环境变量提供认证信息"""
        if not self.email or not self.password:
            print("❌ 缺少认证信息，请设置 BCMAIL_EMAIL 和 BCMAIL_PASSWORD")
            sys.exit(1)
        if not self.provider:
            self.provider = self._resolve_provider()

    def _resolve_provider(self):
        """根据邮箱域名选择服务器配置"""
        domain = self._get_email_domain(self.email)
        provider = SUPPORTED_PROVIDERS.get(domain)
        if provider:
            return provider
        print(f"❌ 不支持的邮箱服务商: {domain}")
        print(f"支持的域名: {', '.join(sorted(SUPPORTED_PROVIDERS))}")
        sys.exit(1)

    def _get_email_domain(self, address):
        """提取邮箱域名"""
        if not address or "@" not in address:
            print("❌ 邮箱地址格式无效，请检查 BCMAIL_EMAIL")
            sys.exit(1)
        return address.rsplit("@", 1)[1].lower()

    def _connect_imap(self):
        """连接 IMAP 服务器"""
        self._ensure_auth()
        try:
            conn = imaplib.IMAP4_SSL(
                self.provider["imap_server"],
                self.provider["imap_port"],
            )
            conn.login(self.email, self.password)
            return conn
        except Exception as e:
            print(f"❌ IMAP连接失败: {e}")
            sys.exit(1)

    def _connect_smtp(self):
        """连接 SMTP 服务器"""
        self._ensure_auth()
        try:
            if self.provider["smtp_port"] == 465:
                conn = smtplib.SMTP_SSL(
                    self.provider["smtp_server"],
                    self.provider["smtp_port"],
                )
            else:
                conn = smtplib.SMTP(
                    self.provider["smtp_server"],
                    self.provider["smtp_port"],
                )
                conn.starttls()
            conn.login(self.email, self.password)
            return conn
        except Exception as e:
            print(f"❌ SMTP连接失败: {e}")
            sys.exit(1)

    def cmd_send(self, args):
        """发送邮件命令"""
        self._ensure_auth()
        to_list = [item.strip() for item in args.to.split(",") if item.strip()]
        cc_list = [item.strip() for item in args.cc.split(",") if item.strip()] if args.cc else []

        msg = MIMEMultipart()
        msg["From"] = self.email
        msg["To"] = ", ".join(to_list)
        msg["Subject"] = args.subject
        if cc_list:
            msg["Cc"] = ", ".join(cc_list)

        msg.attach(MIMEText(args.body, "html" if args.html else "plain", "utf-8"))

        for file_path in args.attach or []:
            if not os.path.exists(file_path):
                print(f"⚠️ 附件不存在，跳过: {file_path}")
                continue
            try:
                with open(file_path, "rb") as handle:
                    part = MIMEBase("application", "octet-stream")
                    part.set_payload(handle.read())
                encoders.encode_base64(part)
                # Header values must not contain CR/LF (header injection).
                filename = (
                    os.path.basename(file_path)
                    .replace("\r", "")
                    .replace("\n", "")
                    .replace('"', "_")
                )
                part.add_header("Content-Disposition", f'attachment; filename="{filename}"')
                msg.attach(part)
                print(f"📎 已添加附件: {filename}")
            except Exception as e:
                print(f"⚠️ 添加附件失败 {file_path}: {e}")

        try:
            conn = self._connect_smtp()
            recipients = to_list + cc_list
            conn.sendmail(self.email, recipients, msg.as_string())
            conn.quit()
            print("✅ 邮件发送成功!")
            print(f"   收件人: {', '.join(to_list)}")
            if cc_list:
                print(f"   抄送: {', '.join(cc_list)}")
            print(f"   主题: {args.subject}")
        except Exception as e:
            print(f"❌ 发送失败: {e}")
            sys.exit(1)

    def cmd_inbox(self, args):
        """查看收件箱命令"""
        conn = self._connect_imap()
        try:
            conn.select("INBOX", readonly=True)
            if args.unread:
                status, data = conn.search(None, "UNSEEN")
                print("📧 未读邮件:")
            else:
                status, data = conn.search(None, "ALL")
                print("📧 收件箱邮件:")

            if status != "OK":
                print("获取邮件列表失败")
                return

            email_ids = data[0].split()
            if not email_ids:
                print("   没有邮件")
                return

            email_ids = email_ids[-args.limit:] if len(email_ids) > args.limit else email_ids
            email_ids.reverse()
            print(f"   显示 {len(email_ids)} 封邮件 (共 {len(data[0].split())} 封)\n")

            for index, eid in enumerate(email_ids, 1):
                self._print_email_summary(conn, eid, index)
        finally:
            conn.close()
            conn.logout()

    def _print_email_summary(self, conn, eid, idx=None):
        """打印邮件摘要"""
        try:
            status, msg_data = conn.fetch(eid, "(RFC822)")
            if status != "OK":
                return

            msg = email.message_from_bytes(msg_data[0][1])
            subject = self._decode_header_str(msg["Subject"]) or "(无主题)"
            from_addr = msg.get("From", "Unknown")
            date_str = msg.get("Date", "")[:20]
            unread_mark = "🔴" if self._is_unread(conn, eid) else "  "
            prefix = f"[{idx}]" if idx else "   "
            print(f"{prefix} {unread_mark} {subject[:40]:<40} | {from_addr[:25]:<25} | {date_str}")
        except Exception as e:
            print(f"   解析邮件 {eid} 失败: {e}")

    def _decode_header_str(self, value):
        """解码邮件头"""
        if not value:
            return ""
        try:
            parts = decode_header(value)
            return "".join(
                part.decode(charset or "utf-8", errors="ignore") if isinstance(part, bytes) else part
                for part, charset in parts
            )
        except Exception:
            return str(value)

    def _is_unread(self, conn, eid):
        """检查邮件是否未读"""
        try:
            status, flags = conn.fetch(eid, "(FLAGS)")
            if status == "OK":
                return b"\\Seen" not in flags[0]
        except Exception:
            pass
        return False

    def cmd_read(self, args):
        """读取指定邮件详情"""
        conn = self._connect_imap()
        try:
            conn.select("INBOX", readonly=True)
            status, msg_data = conn.fetch(args.id, "(RFC822)")
            if status != "OK":
                print(f"❌ 无法获取邮件 {args.id}")
                return

            msg = email.message_from_bytes(msg_data[0][1])
            print("\n" + "=" * 60)
            print(f"📧 邮件 ID: {args.id}")
            print(f"📌 主题: {self._decode_header_str(msg['Subject'])}")
            print(f"👤 发件人: {msg.get('From')}")
            print(f"📅 日期: {msg.get('Date')}")
            print(f"📥 收件人: {msg.get('To')}")
            if msg.get("Cc"):
                print(f"📋 抄送: {msg.get('Cc')}")
            print("=" * 60)
            print(f"\n📝 正文:\n{self._get_body(msg)}")
            print("\n" + "=" * 60)

            attachments = self._get_attachments(msg)
            if attachments:
                print(f"📎 附件: {', '.join(attachments)}")
        finally:
            conn.close()
            conn.logout()

    def _get_body(self, msg):
        """提取邮件正文"""
        body = ""
        if msg.is_multipart():
            for part in msg.walk():
                if part.get_content_disposition() == "attachment":
                    continue
                if part.get_content_type() not in {"text/plain", "text/html"}:
                    continue
                try:
                    payload = part.get_payload(decode=True)
                    if not payload:
                        continue
                    charset = part.get_content_charset() or "utf-8"
                    body = payload.decode(charset, errors="ignore")
                    if part.get_content_type() == "text/plain":
                        break
                except Exception:
                    pass
        else:
            try:
                payload = msg.get_payload(decode=True)
                if payload:
                    charset = msg.get_content_charset() or "utf-8"
                    body = payload.decode(charset, errors="ignore")
            except Exception:
                pass
        return body.strip() if body else "[无正文内容]"

    def _get_attachments(self, msg):
        """获取附件列表"""
        attachments = []
        if msg.is_multipart():
            for part in msg.walk():
                if part.get_content_disposition() == "attachment":
                    filename = part.get_filename()
                    if filename:
                        attachments.append(self._decode_header_str(filename))
        return attachments

    def cmd_search(self, args):
        """搜索邮件命令"""
        conn = self._connect_imap()
        try:
            conn.select("INBOX", readonly=True)
            # IMAP SEARCH conditions are quoted; strip quotes and CR/LF so the
            # keyword cannot close the string and rewrite the search criteria.
            keyword = (
                str(args.keyword)
                .replace("\r", " ")
                .replace("\n", " ")
                .replace('"', " ")
            )
            print(f"🔍 搜索关键词: '{keyword}'")
            status, data = conn.search(None, f'SUBJECT "{keyword}"')
            if status != "OK" or not data[0]:
                status, data = conn.search(None, f'BODY "{keyword}"')
            if status != "OK" or not data[0]:
                print("   未找到相关邮件")
                return

            email_ids = data[0].split()
            print(f"   找到 {len(email_ids)} 封邮件\n")
            for eid in email_ids[-20:]:
                self._print_email_summary(conn, eid)
        finally:
            conn.close()
            conn.logout()

    def cmd_unread(self, args):
        """查看未读邮件数量"""
        conn = self._connect_imap()
        try:
            conn.select("INBOX")
            status, data = conn.search(None, "UNSEEN")
            if status == "OK":
                print(f"📧 未读邮件: {len(data[0].split())} 封")
            else:
                print("❌ 获取失败")
        finally:
            conn.close()
            conn.logout()

    def cmd_folders(self, args):
        """列出所有文件夹"""
        conn = self._connect_imap()
        try:
            status, folders = conn.list()
            if status == "OK":
                print("📁 邮件文件夹:")
                for folder in folders:
                    match = re.search(r'"([^"]+)"$', folder.decode())
                    if not match:
                        continue
                    name = match.group(1)
                    try:
                        conn.select(name, readonly=True)
                        status, data = conn.search(None, "ALL")
                        count = len(data[0].split()) if status == "OK" else 0
                        print(f"   📂 {name:<20} ({count} 封)")
                    except Exception:
                        print(f"   📂 {name}")
        finally:
            conn.close()
            conn.logout()


def main():
    parser = argparse.ArgumentParser(
        description="通用邮箱 CLI 工具",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
环境变量:
  export BCMAIL_EMAIL="you@example.com"
  export BCMAIL_PASSWORD="password-or-app-code"

示例:
  python bcmail.py send --to friend@qq.com --subject "Hello" --body "World"
  python bcmail.py send --to a@gmail.com,b@outlook.com --subject "Hi" --html --body "<h1>Test</h1>"
  python bcmail.py send --to boss@163.com --subject "Report" --attach report.pdf --body "Please check"

  python bcmail.py inbox
  python bcmail.py inbox --limit 5
  python bcmail.py inbox --unread
  python bcmail.py read --id 123
  python bcmail.py search --keyword "会议"
  python bcmail.py unread
  python bcmail.py folders
        """,
    )

    subparsers = parser.add_subparsers(dest="command", help="可用命令")

    send_parser = subparsers.add_parser("send", help="发送邮件")
    send_parser.add_argument("--to", required=True, help="收件人地址，多个用逗号分隔")
    send_parser.add_argument("--subject", required=True, help="邮件主题")
    send_parser.add_argument("--body", required=True, help="邮件正文")
    send_parser.add_argument("--html", action="store_true", help="使用HTML格式")
    send_parser.add_argument("--cc", help="抄送地址，多个用逗号分隔")
    send_parser.add_argument("--attach", nargs="+", help="附件文件路径")

    inbox_parser = subparsers.add_parser("inbox", help="查看收件箱")
    inbox_parser.add_argument("--limit", type=int, default=10, help="显示数量 (默认10)")
    inbox_parser.add_argument("--unread", action="store_true", help="只显示未读邮件")

    read_parser = subparsers.add_parser("read", help="读取邮件详情")
    read_parser.add_argument("--id", required=True, help="邮件ID")

    search_parser = subparsers.add_parser("search", help="搜索邮件")
    search_parser.add_argument("--keyword", required=True, help="搜索关键词")

    subparsers.add_parser("unread", help="查看未读邮件数量")
    subparsers.add_parser("folders", help="列出所有文件夹")

    args = parser.parse_args()
    if not args.command:
        parser.print_help()
        sys.exit(1)

    cli = BCMailCLI()
    commands = {
        "send": cli.cmd_send,
        "inbox": cli.cmd_inbox,
        "read": cli.cmd_read,
        "search": cli.cmd_search,
        "unread": cli.cmd_unread,
        "folders": cli.cmd_folders,
    }
    commands[args.command](args)


if __name__ == "__main__":
    main()
