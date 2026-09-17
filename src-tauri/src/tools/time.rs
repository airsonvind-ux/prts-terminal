//! tools::time —— 时间参照工具
//!
//! 极简的无参数查询工具：返回当前本地时间（含星期）、时区、UTC 偏移与
//! ISO 时刻。定位是"零注入"方案的落点——系统提示与上下文中不注入任何
//! 时间信息，需要"现在"时由 LLM 主动调用；记忆系统的时间戳参照同样来源于此。
//!
//! 时区取系统本地（不做配置项）；IANA 时区名经 iana-time-zone 获取
//! （Windows 走 WinRT Calendar），异常时降级为"未知"，不阻断调用。
//! 按 Phase4 规格：不做时间快照、不做日期变更通知。

use chrono::{Datelike, Local, SecondsFormat, Utc, Weekday};

use crate::tools::{Tool, ToolContext, ToolDisplay, ToolError, ToolOutput};

/// 时间参照工具：无参数、无外部依赖，取值由系统本地时钟与本地时区决定。
pub struct TimeTool;

impl Tool for TimeTool {
    fn name(&self) -> &str {
        "time"
    }

    fn description(&self) -> &str {
        "获取当前时间参照：本地时间（含星期）、时区、UTC 偏移、ISO 时刻。无参数。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {}
        })
    }

    fn execute(&self, _ctx: &ToolContext, _params: &str) -> Result<ToolOutput, ToolError> {
        let now = Local::now();
        let iso = now
            .with_timezone(&Utc)
            .to_rfc3339_opts(SecondsFormat::Millis, true);
        let local_date_time = now.format("%Y-%m-%d %H:%M:%S").to_string();
        let utc_offset = now.format("%:z").to_string();
        let weekday = match now.weekday() {
            Weekday::Mon => "星期一",
            Weekday::Tue => "星期二",
            Weekday::Wed => "星期三",
            Weekday::Thu => "星期四",
            Weekday::Fri => "星期五",
            Weekday::Sat => "星期六",
            Weekday::Sun => "星期日",
        };
        let timezone = iana_time_zone::get_timezone().unwrap_or_else(|_| "未知".to_string());

        let mut summary = String::new();
        summary.push_str("━━━ 当前时间 ━━━\n");
        summary.push_str(&format!("本地时间：{local_date_time}\n"));
        summary.push_str(&format!("星期：{weekday}\n"));
        summary.push_str(&format!("时区：{timezone}\n"));
        summary.push_str(&format!("UTC 偏移：{utc_offset}\n"));
        summary.push_str(&format!("ISO：{iso}\n"));

        let canonical = serde_json::json!({
            "localDateTime": local_date_time,
            "weekday": weekday,
            "timezone": timezone,
            "utcOffset": utc_offset,
            "iso": iso,
        });
        Ok(ToolOutput::json(canonical, summary))
    }

    fn display(&self, _ctx: &ToolContext, _params: &str) -> ToolDisplay {
        ToolDisplay::info("查看时间")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;

    /// 取以 prefix 开头的行（不存在则 panic 展示全文）。
    fn line_of<'a>(summary: &'a str, prefix: &str) -> &'a str {
        summary
            .lines()
            .find(|line| line.starts_with(prefix))
            .unwrap_or_else(|| panic!("缺少「{prefix}」行：\n{summary}"))
    }

    #[test]
    fn output_shape() {
        let out = TimeTool.execute(&ToolContext::new(), "{}").unwrap();
        let s = out.summary.as_str();

        // 行齐全
        for prefix in ["本地时间：", "星期：", "时区：", "UTC 偏移：", "ISO："] {
            line_of(s, prefix);
        }

        // 本地时间为 YYYY-MM-DD HH:MM:SS
        let local_re = Regex::new(r"^本地时间：\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}$").unwrap();
        assert!(
            local_re.is_match(line_of(s, "本地时间：")),
            "本地时间格式异常：\n{s}"
        );

        // 星期为合法中文值
        let weekday_line = line_of(s, "星期：");
        assert!(
            ["星期一", "星期二", "星期三", "星期四", "星期五", "星期六", "星期日"]
                .iter()
                .any(|w| weekday_line == format!("星期：{w}")),
            "星期取值异常：\n{s}"
        );

        // UTC 偏移形如 +08:00
        let offset_re = Regex::new(r"^UTC 偏移：[+-]\d{2}:\d{2}$").unwrap();
        assert!(
            offset_re.is_match(line_of(s, "UTC 偏移：")),
            "偏移格式异常：\n{s}"
        );

        // ISO 为 UTC（Z 结尾）
        assert!(line_of(s, "ISO：").ends_with('Z'), "ISO 格式异常：\n{s}");

        // 时区名为真实获取（非降级占位）
        assert_ne!(line_of(s, "时区："), "时区：未知", "时区名获取失败：\n{s}");
    }

    #[test]
    fn canonical_matches_summary() {
        let out = TimeTool.execute(&ToolContext::new(), "{}").unwrap();

        for key in ["localDateTime", "weekday", "timezone", "utcOffset", "iso"] {
            let value = out
                .canonical
                .get(key)
                .and_then(|v| v.as_str())
                .unwrap_or_else(|| panic!("canonical.{key} 应为字符串"));
            assert!(!value.is_empty(), "canonical.{key} 为空");
            assert!(
                out.summary.contains(value),
                "canonical.{key}={value} 未出现在文本中"
            );
        }
    }

    #[test]
    fn schema_shape() {
        let schema = TimeTool.parameters();
        assert!(crate::tools::schema::validate_tool_schema(&schema).is_ok());
        assert!(crate::tools::schema::validate_arguments(&schema, &serde_json::json!({})).is_ok());
    }
}
