# 拍照技能 (Camera Capture)

## 触发语句
- "帮我拍张照" / "拍照" / "拍一张" / "照相" / "自拍"
- "take a photo" / "capture photo"
- "用摄像头拍一张"

## 参数澄清
当用户请求拍照时，如果以下信息缺失，**先询问**：
1. **保存位置** — 默认保存到 `~/.blockcell/workspace/media/photo_<时间戳>.jpg`，用户可指定路径
2. **图片格式** — 默认 jpg，可选 png、tiff
3. **摄像头选择** — 默认使用第一个摄像头（index=0），如果有多个摄像头可询问

如果用户只说"拍张照"，**不需要询问**，直接使用默认参数拍照。

## 工具调用顺序
1. `camera_capture(action="info")` — 获取摄像头信息和可用拍摄方式（仅首次或用户询问时）
2. `camera_capture(action="list")` — 列出可用摄像头（仅用户需要选择时）
3. `camera_capture(action="capture", device_index=0, format="jpg")` — 执行拍照

## 输出格式
```markdown
📷 拍照完成！

- **文件路径**: /path/to/photo.jpg
- **文件大小**: 123 KB
- **拍摄方式**: ffmpeg_avfoundation
- **摄像头**: FaceTime HD Camera

[如果需要，我可以帮你查看、编辑或发送这张照片]
```

## 失败与降级策略
1. **ffmpeg 不可用** → 尝试 imagecapture → 尝试 screencapture（截屏，非摄像头）
2. **所有方式都失败** → 告知用户安装 ffmpeg：`brew install ffmpeg`
3. **摄像头被占用** → 提示用户关闭其他使用摄像头的应用
4. **权限被拒绝** → 提示用户在系统偏好设置中授权终端访问摄像头

## 示例

### 示例 1：简单拍照
**用户**: 帮我拍张照
**助手**: 
1. 调用 `camera_capture(action="capture")`
2. 返回拍照结果，包含文件路径和大小

### 示例 2：指定格式和路径
**用户**: 拍一张 PNG 格式的照片，保存到桌面
**助手**:
1. 调用 `camera_capture(action="capture", format="png", output_path="~/Desktop/photo.png")`
2. 返回结果

### 示例 3：多摄像头选择
**用户**: 我想用外接摄像头拍照
**助手**:
1. 调用 `camera_capture(action="list")` 列出所有摄像头
2. 展示摄像头列表，让用户选择
3. 用户选择后，调用 `camera_capture(action="capture", device_index=<选择的索引>)`
