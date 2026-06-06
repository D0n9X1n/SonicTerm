# Logging / 日志

## English

Log path: `~/.sonicterm/logs/sonicterm.log`

Crash dumps: `~/.sonicterm/logs/crashes/`

Config:

```toml
[logging]
level = "info"
max_file_size_mb = 10
max_rotated_files = 3
max_age_days = 2
max_crash_dumps = 10
max_crash_age_days = 2
```

By default, SonicTerm cleans logs and crash dumps older than 2 days.

For bug reports, attach the last 200 lines of the newest log and a screenshot for
rendering or input bugs.

## 中文

日志路径：`~/.sonicterm/logs/sonicterm.log`

Crash dumps / 崩溃日志：`~/.sonicterm/logs/crashes/`

默认会自动清理 2 天以上的日志和崩溃日志。日志配置见上方 TOML 示例。提交 bug
时请附上最新日志最后 200 行；渲染或输入问题请附截图。
