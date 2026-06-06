# Logging / 日志

## English

Log paths:

- macOS: `~/Library/Logs/SonicTerm/sonicterm.log`
- Windows: `%LOCALAPPDATA%\SonicTerm\logs\sonicterm.log`

Config:

```toml
[logging]
level = "info"
max_files = 8
max_bytes = 1048576
```

For bug reports, attach the last 200 lines of the newest log and a screenshot
for rendering or input bugs.

## 中文

日志路径：

- macOS: `~/Library/Logs/SonicTerm/sonicterm.log`
- Windows: `%LOCALAPPDATA%\SonicTerm\logs\sonicterm.log`

日志配置见上方 TOML 示例。提交 bug 时请附上最新日志最后 200 行；渲染或输入
问题请附截图。
