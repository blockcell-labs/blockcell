#!/usr/bin/env python3
"""
WeCom (企业微信) AES-256-CBC 解密测试脚本
用法: python3 test_wecom_decrypt.py

把你的 encoding_aes_key 和从日志里复制出来的 echostr / msg_encrypt 填进去，
脚本会逐步输出每个阶段的中间值，帮助定位解密失败的原因。
"""

import base64
import struct
import sys
from Crypto.Cipher import AES  # pip install pycryptodome

# ─── 填入你的配置 ────────────────────────────────────────────────────────────
ENCODING_AES_KEY = "请把企业微信后台的EncodingAESKey粘贴到这里"   # 43字符，不含'='
MSG_ENCRYPT      = "请把日志里输出的 msg_encrypt_raw 或 echostr_raw 粘贴到这里"
# ─────────────────────────────────────────────────────────────────────────────


def pkcs7_unpad(data: bytes) -> bytes:
    pad_len = data[-1]
    return data[:-pad_len]


def decrypt_wecom(encoding_aes_key: str, msg_encrypt: str) -> str:
    print("=" * 60)
    print(f"[1] encoding_aes_key 原始值 (len={len(encoding_aes_key)}):")
    print(f"    '{encoding_aes_key}'")

    # 去掉首尾空白
    key_stripped = encoding_aes_key.strip()
    print(f"\n[2] strip() 后 (len={len(key_stripped)}):")
    print(f"    '{key_stripped}'")

    # 去掉末尾 '='
    key_no_pad = key_stripped.rstrip('=')
    print(f"\n[3] rstrip('=') 后 (len={len(key_no_pad)}):")
    print(f"    '{key_no_pad}'")

    # 补齐 base64 padding
    remainder = len(key_no_pad) % 4
    if remainder == 0:
        padded = key_no_pad
    elif remainder == 2:
        padded = key_no_pad + '=='
    elif remainder == 3:
        padded = key_no_pad + '='
    else:
        raise ValueError(f"无效的 EncodingAESKey 长度 {len(key_no_pad)}，不是有效的 base64")

    print(f"\n[4] 补 padding 后 (len={len(padded)}):")
    print(f"    '{padded}'")

    # base64 解码 AES key
    try:
        key_bytes = base64.b64decode(padded)
    except Exception as e:
        print(f"\n[ERROR] base64 解码 EncodingAESKey 失败: {e}")
        sys.exit(1)

    print(f"\n[5] AES key bytes (len={len(key_bytes)}):")
    print(f"    {key_bytes.hex()}")

    if len(key_bytes) != 32:
        print(f"\n[ERROR] AES key 长度应为 32 字节，实际 {len(key_bytes)} 字节")
        sys.exit(1)

    iv = key_bytes[:16]
    print(f"\n[6] IV (前16字节): {iv.hex()}")

    # 解码 msg_encrypt
    print(f"\n[7] msg_encrypt 原始值 (len={len(msg_encrypt)}):")
    print(f"    '{msg_encrypt}'")

    # 检查是否含有空格（URL解码问题：+ → 空格）
    if ' ' in msg_encrypt:
        print("\n[WARNING] msg_encrypt 含有空格！这通常是 URL 解码把 '+' 变成空格导致的。")
        print("          修复方法：把空格替换回 '+'")
        msg_encrypt_fixed = msg_encrypt.replace(' ', '+')
        print(f"          修复后: '{msg_encrypt_fixed}'")
    else:
        msg_encrypt_fixed = msg_encrypt

    try:
        ciphertext = base64.b64decode(msg_encrypt_fixed)
    except Exception as e:
        print(f"\n[ERROR] base64 解码 msg_encrypt 失败: {e}")
        sys.exit(1)

    print(f"\n[8] ciphertext bytes (len={len(ciphertext)}): {ciphertext[:16].hex()}...")

    if len(ciphertext) % 16 != 0:
        print(f"\n[ERROR] ciphertext 长度 {len(ciphertext)} 不是 16 的倍数，AES 块对齐失败")
        sys.exit(1)

    # AES-256-CBC 解密
    cipher = AES.new(key_bytes, AES.MODE_CBC, iv)
    try:
        plaintext_padded = cipher.decrypt(ciphertext)
    except Exception as e:
        print(f"\n[ERROR] AES 解密失败: {e}")
        sys.exit(1)

    print(f"\n[9] 解密后（含 PKCS7 padding）前32字节: {plaintext_padded[:32].hex()}")

    plaintext = pkcs7_unpad(plaintext_padded)
    print(f"\n[10] 去掉 padding 后 (len={len(plaintext)}):")

    if len(plaintext) < 20:
        print(f"[ERROR] 解密结果太短: {len(plaintext)} 字节")
        sys.exit(1)

    # 解析结构: 16B random | 4B msg_len (big-endian) | msg | corpId
    random_bytes = plaintext[:16]
    msg_len = struct.unpack('>I', plaintext[16:20])[0]
    print(f"\n[11] random bytes: {random_bytes.hex()}")
    print(f"[12] msg_len (big-endian): {msg_len}")

    if 20 + msg_len > len(plaintext):
        print(f"[ERROR] msg_len={msg_len} 超出 plaintext 长度 {len(plaintext)}")
        sys.exit(1)

    msg = plaintext[20:20 + msg_len].decode('utf-8')
    corp_id = plaintext[20 + msg_len:].decode('utf-8', errors='replace')

    print(f"\n[13] 解密成功！")
    print(f"     msg    = '{msg}'")
    print(f"     corpId = '{corp_id}'")
    print("=" * 60)
    return msg


if __name__ == '__main__':
    if ENCODING_AES_KEY.startswith("请把"):
        print("请先编辑脚本，填入 ENCODING_AES_KEY 和 MSG_ENCRYPT")
        sys.exit(1)
    decrypt_wecom(ENCODING_AES_KEY, MSG_ENCRYPT)
