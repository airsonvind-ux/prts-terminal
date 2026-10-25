//! 会话 id 文件名安全校验（单一事实源）。
//!
//! 同一个 conversationId 会同时作为 `{id}.jsonl`、`.prefix.json`、
//! `.skills.txt`、`.memory.txt` 的文件名，因此所有持久化边界必须使用同一规则。
//! 规则保持原有行为：非空，仅允许 ASCII 字母数字与 `-` `_`；`.` 和路径分隔符
//! 都不在允许字符内。

/// conversationId / sessionId 是否可安全用作文件名。
pub(crate) fn is_safe_session_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_generated_and_uuid_shapes() {
        for id in [
            "a",
            "A1",
            "conv-1",
            "conv_1",
            "550e8400-e29b-41d4-a716-446655440000",
        ] {
            assert!(is_safe_session_id(id), "应接受 {id:?}");
        }
    }

    #[test]
    fn rejects_empty_and_path_like_ids() {
        for id in [
            "", ".", "..", ".hidden", "a/b", "a\\b", "a b", "中文", "a:b", "a.b",
        ] {
            assert!(!is_safe_session_id(id), "应拒绝 {id:?}");
        }
    }
}
