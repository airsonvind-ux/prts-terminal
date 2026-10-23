# Provider Compatibility Matrix

> Generated from `src-tauri/resources/models.json` by `src-tauri/src/llm/snapshot_tests.rs`.
> This is the current effective contract, not a support claim. `U` means not modeled yet.

- Models: 195
- Reasoning: 108
- Reasoning without explicit thinking levels: 95
- Models with model-level compat: 15
- Models with thinking profile: 6
- `maxOutput`: known 179 / not applicable 2 / unverified 14
- `thinkingProfile`: U for 189 models
- `clearable=true`: 14 models
- `assistant_reasoning=required`: 3 models
- `stream_usage`: supported 106 / unsupported 14 / unknown 75

| provider | model | protocol | context | reasoning | vision | thinking | levels (default) | policy | carrier | param | stream | max output | profile |
|---|---|---|---:|---|---|---|---|---|---|---|---|---|---|
| anthropic | claude-4-opus-20250514 | anthropic-messages | 200000 | yes | yes | anthropic | - | preserve | thinking_blocks | - | omit | 32000 | U |
| anthropic | claude-4-sonnet-20250514 | anthropic-messages | 1000000 | yes | yes | anthropic | - | preserve | thinking_blocks | - | omit | 64000 | U |
| anthropic | claude-fable-5 | anthropic-messages | 1000000 | yes | yes | anthropic | - | preserve | thinking_blocks | - | omit | 128000 | U |
| anthropic | claude-haiku-4-5 | anthropic-messages | 200000 | yes | yes | anthropic | - | preserve | thinking_blocks | - | omit | 64000 | U |
| anthropic | claude-haiku-4-5-20251001 | anthropic-messages | 200000 | yes | yes | anthropic | - | preserve | thinking_blocks | - | omit | 64000 | U |
| anthropic | claude-mythos-5 | anthropic-messages | 1000000 | yes | yes | anthropic | - | preserve | thinking_blocks | - | omit | 128000 | U |
| anthropic | claude-opus-4-1 | anthropic-messages | 200000 | yes | yes | anthropic | - | preserve | thinking_blocks | - | omit | 32000 | U |
| anthropic | claude-opus-4-1-20250805 | anthropic-messages | 200000 | yes | yes | anthropic | - | preserve | thinking_blocks | - | omit | 32000 | U |
| anthropic | claude-opus-4-20250514 | anthropic-messages | 200000 | yes | yes | anthropic | - | preserve | thinking_blocks | - | omit | 32000 | U |
| anthropic | claude-opus-4-5 | anthropic-messages | 200000 | yes | yes | anthropic | - | preserve | thinking_blocks | - | omit | 64000 | U |
| anthropic | claude-opus-4-5-20251101 | anthropic-messages | 200000 | yes | yes | anthropic | - | preserve | thinking_blocks | - | omit | 64000 | U |
| anthropic | claude-opus-4-6 | anthropic-messages | 200000 | yes | yes | anthropic | - | preserve | thinking_blocks | - | omit | 128000 | U |
| anthropic | claude-opus-4-6-20260205 | anthropic-messages | 1000000 | yes | yes | anthropic | - | preserve | thinking_blocks | - | omit | 128000 | U |
| anthropic | claude-opus-4-7 | anthropic-messages | 1000000 | yes | yes | anthropic | - | preserve | thinking_blocks | - | omit | 128000 | U |
| anthropic | claude-opus-5 | anthropic-messages | 1000000 | yes | yes | anthropic | - | preserve | thinking_blocks | - | omit | 128000 | U |
| anthropic | claude-sonnet-4-20250514 | anthropic-messages | 200000 | yes | yes | anthropic | - | preserve | thinking_blocks | - | omit | 64000 | U |
| anthropic | claude-sonnet-4-5 | anthropic-messages | 200000 | yes | yes | anthropic | - | preserve | thinking_blocks | - | omit | 64000 | U |
| anthropic | claude-sonnet-4-5-20250929 | anthropic-messages | 200000 | yes | yes | anthropic | - | preserve | thinking_blocks | - | omit | 64000 | U |
| anthropic | claude-sonnet-4-6 | anthropic-messages | 200000 | yes | yes | anthropic | - | preserve | thinking_blocks | - | omit | 64000 | U |
| anthropic | claude-sonnet-5 | anthropic-messages | 1000000 | yes | yes | anthropic | - | preserve | thinking_blocks | - | omit | 128000 | U |
| deepseek | deepseek-flash | openai-completions | 1000000 | yes | yes | deepseek | off,low,high,max (high) | require_tool_call | reasoning_content | thinking | send | U | U |
| deepseek | deepseek-v4-flash | openai-completions | 1000000 | yes | no | deepseek | off,low,high,max (high) | require_tool_call | reasoning_content | thinking | send | 384000 | U |
| deepseek | deepseek-v4-pro | openai-completions | 1000000 | yes | no | deepseek | off,low,high,max (high) | require_tool_call | reasoning_content | thinking | send | 384000 | U |
| gemini | gemini-2.0-flash | openai-completions | 1048576 | no | yes | - | - | preserve | - | - | omit | 8192 | U |
| gemini | gemini-2.0-flash-001 | openai-completions | 1048576 | no | yes | - | - | preserve | - | - | omit | 8192 | U |
| gemini | gemini-2.0-flash-lite | openai-completions | 1048576 | no | yes | - | - | preserve | - | - | omit | 8192 | U |
| gemini | gemini-2.0-flash-lite-001 | openai-completions | 1048576 | no | yes | - | - | preserve | - | - | omit | 8192 | U |
| gemini | gemini-2.5-computer-use-preview-10-2025 | openai-completions | 128000 | no | yes | - | - | preserve | - | - | omit | 64000 | U |
| gemini | gemini-2.5-flash | openai-completions | 1048576 | yes | yes | - | - | preserve | - | - | omit | 65536 | U |
| gemini | gemini-2.5-flash-lite | openai-completions | 1048576 | yes | yes | - | - | preserve | - | - | omit | 65535 | U |
| gemini | gemini-2.5-flash-lite-preview-06-17 | openai-completions | 1048576 | yes | yes | - | - | preserve | - | - | omit | 65535 | U |
| gemini | gemini-2.5-flash-lite-preview-09-2025 | openai-completions | 1048576 | yes | yes | - | - | preserve | - | - | omit | 65535 | U |
| gemini | gemini-2.5-flash-native-audio-latest | openai-completions | 1048576 | no | no | - | - | preserve | - | - | omit | 8192 | U |
| gemini | gemini-2.5-flash-native-audio-preview-09-2025 | openai-completions | 1048576 | no | no | - | - | preserve | - | - | omit | 8192 | U |
| gemini | gemini-2.5-flash-native-audio-preview-12-2025 | openai-completions | 1048576 | no | no | - | - | preserve | - | - | omit | 8192 | U |
| gemini | gemini-2.5-flash-preview-09-2025 | openai-completions | 1048576 | yes | yes | - | - | preserve | - | - | omit | 65535 | U |
| gemini | gemini-2.5-pro | openai-completions | 1048576 | yes | yes | - | - | preserve | - | - | omit | 65536 | U |
| gemini | gemini-2.5-pro-preview-tts | openai-completions | 1048576 | no | yes | - | - | preserve | - | - | omit | 65535 | U |
| gemini | gemini-3-flash-preview | openai-completions | 1048576 | yes | yes | - | - | preserve | - | - | omit | 65535 | U |
| gemini | gemini-3-pro-preview | openai-completions | 1048576 | yes | yes | - | - | preserve | - | - | omit | 65535 | U |
| gemini | gemini-3.1-flash-lite-preview | openai-completions | 1048576 | yes | yes | - | - | preserve | - | - | omit | 65536 | U |
| gemini | gemini-3.1-pro-preview | openai-completions | 1048576 | yes | yes | - | - | preserve | - | - | omit | 65536 | U |
| gemini | gemini-3.1-pro-preview-customtools | openai-completions | 1048576 | yes | yes | - | - | preserve | - | - | omit | 65536 | U |
| gemini | gemini-exp-1114 | openai-completions | 1048576 | no | yes | - | - | preserve | - | - | omit | 8192 | U |
| gemini | gemini-exp-1206 | openai-completions | 2097152 | no | yes | - | - | preserve | - | - | omit | 8192 | U |
| gemini | gemini-flash-latest | openai-completions | 1048576 | yes | yes | - | - | preserve | - | - | omit | 65535 | U |
| gemini | gemini-flash-lite-latest | openai-completions | 1048576 | yes | yes | - | - | preserve | - | - | omit | 65535 | U |
| gemini | gemini-gemma-2-27b-it | openai-completions | 8192 | no | yes | - | - | preserve | - | - | omit | 8192 | U |
| gemini | gemini-gemma-2-9b-it | openai-completions | 8192 | no | yes | - | - | preserve | - | - | omit | 8192 | U |
| gemini | gemini-pro-latest | openai-completions | 1048576 | yes | yes | - | - | preserve | - | - | omit | 65535 | U |
| gemini | gemini-robotics-er-1.5-preview | openai-completions | 1048576 | yes | yes | - | - | preserve | - | - | omit | 65535 | U |
| gemini | gemma-3-27b-it | openai-completions | 131072 | no | yes | - | - | preserve | - | - | omit | 8192 | U |
| gemini | learnlm-1.5-pro-experimental | openai-completions | 32767 | no | yes | - | - | preserve | - | - | omit | 8192 | U |
| kimi | kimi-k2.6 | openai-completions | 262144 | yes | yes | kimi | off,high (high) | preserve | reasoning_content | kimi | send | 98304 | kimi-k2.6 |
| kimi | kimi-k2.7-code | openai-completions | 262144 | yes | no | kimi | high (high) | preserve | reasoning_content | kimi | send | U | kimi-k2.7-code |
| kimi | kimi-k3 | openai-completions | 1048576 | yes | yes | kimi | low,high,max (max) | preserve | reasoning_content | kimi | send | U | kimi-k3 |
| ollama | deepseek-r1 | openai-completions | 131072 | yes | no | - | - | preserve | - | - | omit | 8192 | U |
| ollama | deepseek-v3.1:671b-cloud | openai-completions | 163840 | no | no | - | - | preserve | - | - | omit | 163840 | U |
| ollama | gemma3 | openai-completions | 131072 | no | yes | - | - | preserve | - | - | omit | 8192 | U |
| ollama | gpt-oss:120b-cloud | openai-completions | 131072 | no | no | - | - | preserve | - | - | omit | 131072 | U |
| ollama | gpt-oss:20b-cloud | openai-completions | 131072 | no | no | - | - | preserve | - | - | omit | 131072 | U |
| ollama | llama3.1 | openai-completions | 131072 | no | no | - | - | preserve | - | - | omit | 8192 | U |
| ollama | llama3.2 | openai-completions | 131072 | no | no | - | - | preserve | - | - | omit | 8192 | U |
| ollama | mistral | openai-completions | 32768 | no | no | - | - | preserve | - | - | omit | 8192 | U |
| ollama | qwen2.5 | openai-completions | 131072 | no | no | - | - | preserve | - | - | omit | 8192 | U |
| ollama | qwen3 | openai-completions | 131072 | no | no | - | - | preserve | - | - | omit | 8192 | U |
| ollama | qwen3-coder:480b-cloud | openai-completions | 262144 | no | no | - | - | preserve | - | - | omit | 262144 | U |
| openai | chatgpt-4o-latest | openai-completions | 128000 | no | yes | - | - | preserve | - | - | send | 4096 | U |
| openai | ft:gpt-4.1-2025-04-14 | openai-completions | 1047576 | no | no | - | - | preserve | - | - | send | 32768 | U |
| openai | ft:gpt-4o-mini-2024-07-18 | openai-completions | 128000 | no | no | - | - | preserve | - | - | send | 16384 | U |
| openai | gpt-3.5-turbo-1106 | openai-completions | 16385 | no | no | - | - | preserve | - | - | send | 4096 | U |
| openai | gpt-4.1 | openai-completions | 1047576 | no | yes | - | - | preserve | - | - | send | 32768 | U |
| openai | gpt-4.1-2025-04-14 | openai-completions | 1047576 | no | yes | - | - | preserve | - | - | send | 32768 | U |
| openai | gpt-4.1-mini | openai-completions | 1047576 | no | yes | - | - | preserve | - | - | send | 32768 | U |
| openai | gpt-4.1-mini-2025-04-14 | openai-completions | 1047576 | no | yes | - | - | preserve | - | - | send | 32768 | U |
| openai | gpt-4.1-nano | openai-completions | 1047576 | no | yes | - | - | preserve | - | - | send | 32768 | U |
| openai | gpt-4.1-nano-2025-04-14 | openai-completions | 1047576 | no | yes | - | - | preserve | - | - | send | 32768 | U |
| openai | gpt-4o | openai-completions | 128000 | no | yes | - | - | preserve | - | - | send | 16384 | U |
| openai | gpt-4o-2024-05-13 | openai-completions | 128000 | no | yes | - | - | preserve | - | - | send | 4096 | U |
| openai | gpt-4o-2024-08-06 | openai-completions | 128000 | no | yes | - | - | preserve | - | - | send | 16384 | U |
| openai | gpt-4o-2024-11-20 | openai-completions | 128000 | no | yes | - | - | preserve | - | - | send | 16384 | U |
| openai | gpt-4o-audio-preview | openai-completions | 128000 | no | no | - | - | preserve | - | - | send | 16384 | U |
| openai | gpt-4o-audio-preview-2024-12-17 | openai-completions | 128000 | no | no | - | - | preserve | - | - | send | 16384 | U |
| openai | gpt-4o-audio-preview-2025-06-03 | openai-completions | 128000 | no | no | - | - | preserve | - | - | send | 16384 | U |
| openai | gpt-4o-mini | openai-completions | 128000 | no | yes | - | - | preserve | - | - | send | 16384 | U |
| openai | gpt-4o-mini-2024-07-18 | openai-completions | 128000 | no | yes | - | - | preserve | - | - | send | 16384 | U |
| openai | gpt-4o-mini-audio-preview | openai-completions | 128000 | no | no | - | - | preserve | - | - | send | 16384 | U |
| openai | gpt-4o-mini-audio-preview-2024-12-17 | openai-completions | 128000 | no | no | - | - | preserve | - | - | send | 16384 | U |
| openai | gpt-4o-mini-realtime-preview | openai-completions | 128000 | no | no | - | - | preserve | - | - | send | 4096 | U |
| openai | gpt-4o-mini-realtime-preview-2024-12-17 | openai-completions | 128000 | no | no | - | - | preserve | - | - | send | 4096 | U |
| openai | gpt-4o-mini-search-preview | openai-completions | 128000 | no | yes | - | - | preserve | - | - | send | 16384 | U |
| openai | gpt-4o-mini-search-preview-2025-03-11 | openai-completions | 128000 | no | yes | - | - | preserve | - | - | send | 16384 | U |
| openai | gpt-4o-realtime-preview | openai-completions | 128000 | no | no | - | - | preserve | - | - | send | 4096 | U |
| openai | gpt-4o-realtime-preview-2024-12-17 | openai-completions | 128000 | no | no | - | - | preserve | - | - | send | 4096 | U |
| openai | gpt-4o-realtime-preview-2025-06-03 | openai-completions | 128000 | no | no | - | - | preserve | - | - | send | 4096 | U |
| openai | gpt-4o-search-preview | openai-completions | 128000 | no | yes | - | - | preserve | - | - | send | 16384 | U |
| openai | gpt-4o-search-preview-2025-03-11 | openai-completions | 128000 | no | yes | - | - | preserve | - | - | send | 16384 | U |
| openai | gpt-5 | openai-completions | 400000 | yes | yes | - | - | preserve | - | - | send | 128000 | U |
| openai | gpt-5-2025-08-07 | openai-completions | 272000 | yes | yes | - | - | preserve | - | - | send | 128000 | U |
| openai | gpt-5-chat | openai-completions | 128000 | yes | yes | - | - | preserve | - | - | send | 16384 | U |
| openai | gpt-5-chat-latest | openai-completions | 128000 | no | yes | - | - | preserve | - | - | send | 16384 | U |
| openai | gpt-5-mini | openai-completions | 400000 | yes | yes | - | - | preserve | - | - | send | 128000 | U |
| openai | gpt-5-mini-2025-08-07 | openai-completions | 272000 | yes | yes | - | - | preserve | - | - | send | 128000 | U |
| openai | gpt-5-nano | openai-completions | 400000 | yes | yes | - | - | preserve | - | - | send | 128000 | U |
| openai | gpt-5-nano-2025-08-07 | openai-completions | 272000 | yes | yes | - | - | preserve | - | - | send | 128000 | U |
| openai | gpt-5-search-api | openai-completions | 272000 | no | yes | - | - | preserve | - | - | send | 128000 | U |
| openai | gpt-5-search-api-2025-10-14 | openai-completions | 272000 | no | yes | - | - | preserve | - | - | send | 128000 | U |
| openai | gpt-5.1 | openai-completions | 400000 | yes | yes | - | - | preserve | - | - | send | 128000 | U |
| openai | gpt-5.1-2025-11-13 | openai-completions | 272000 | yes | yes | - | - | preserve | - | - | send | 128000 | U |
| openai | gpt-5.1-chat-latest | openai-completions | 128000 | yes | yes | - | - | preserve | - | - | send | 16384 | U |
| openai | gpt-5.1-codex-mini | openai-completions | 400000 | yes | yes | - | - | preserve | - | - | send | 128000 | U |
| openai | gpt-5.2 | openai-completions | 400000 | yes | yes | - | - | preserve | - | - | send | 128000 | U |
| openai | gpt-5.2-2025-12-11 | openai-completions | 272000 | yes | yes | - | - | preserve | - | - | send | 128000 | U |
| openai | gpt-5.2-chat-latest | openai-completions | 128000 | yes | yes | - | - | preserve | - | - | send | 16384 | U |
| openai | gpt-5.2-codex | openai-completions | 400000 | yes | yes | - | - | preserve | - | - | send | 128000 | U |
| openai | gpt-5.3-chat-latest | openai-completions | 128000 | no | yes | - | - | preserve | - | - | send | 16384 | U |
| openai | gpt-5.3-codex | openai-completions | 400000 | yes | yes | - | - | preserve | - | - | send | 128000 | U |
| openai | gpt-5.3-codex-spark | openai-completions | 128000 | yes | yes | - | - | preserve | - | - | send | 32000 | U |
| openai | gpt-5.4 | openai-completions | 272000 | yes | yes | - | - | preserve | - | - | send | 128000 | U |
| openai | gpt-5.4-2026-03-05 | openai-completions | 1050000 | yes | yes | - | - | preserve | - | - | send | 128000 | U |
| openai | gpt-5.4-mini | openai-completions | 400000 | yes | yes | - | - | preserve | - | - | send | 128000 | U |
| openai | gpt-5.4-nano | openai-completions | 400000 | yes | yes | - | - | preserve | - | - | send | 128000 | U |
| openai | gpt-5.5 | openai-completions | 272000 | yes | yes | - | - | preserve | - | - | send | 128000 | U |
| openai | gpt-5.6 | openai-completions | 1050000 | yes | yes | - | off,low,medium,high,max (medium) | preserve | - | - | send | 128000 | U |
| openai | gpt-5.6-luna | openai-completions | 1050000 | yes | yes | - | off,low,medium,high,max (medium) | preserve | - | - | send | 128000 | U |
| openai | gpt-5.6-sol | openai-completions | 1050000 | yes | yes | - | off,low,medium,high,max (medium) | preserve | - | - | send | 128000 | U |
| openai | gpt-5.6-terra | openai-completions | 1050000 | yes | yes | - | off,low,medium,high,max (medium) | preserve | - | - | send | 128000 | U |
| openai | gpt-audio | openai-completions | 128000 | no | no | - | - | preserve | - | - | send | 16384 | U |
| openai | gpt-audio-1.5 | openai-completions | 128000 | no | no | - | - | preserve | - | - | send | 16384 | U |
| openai | gpt-audio-2025-08-28 | openai-completions | 128000 | no | no | - | - | preserve | - | - | send | 16384 | U |
| openai | gpt-audio-mini | openai-completions | 128000 | no | no | - | - | preserve | - | - | send | 16384 | U |
| openai | gpt-audio-mini-2025-10-06 | openai-completions | 128000 | no | no | - | - | preserve | - | - | send | 16384 | U |
| openai | gpt-audio-mini-2025-12-15 | openai-completions | 128000 | no | no | - | - | preserve | - | - | send | 16384 | U |
| openai | gpt-realtime | openai-completions | 32000 | no | no | - | - | preserve | - | - | send | 4096 | U |
| openai | gpt-realtime-1.5 | openai-completions | 32000 | no | no | - | - | preserve | - | - | send | 4096 | U |
| openai | gpt-realtime-2025-08-28 | openai-completions | 32000 | no | no | - | - | preserve | - | - | send | 4096 | U |
| openai | gpt-realtime-mini | openai-completions | 128000 | no | no | - | - | preserve | - | - | send | 4096 | U |
| openai | gpt-realtime-mini-2025-10-06 | openai-completions | 128000 | no | no | - | - | preserve | - | - | send | 4096 | U |
| openai | gpt-realtime-mini-2025-12-15 | openai-completions | 128000 | no | no | - | - | preserve | - | - | send | 4096 | U |
| openai | o1 | openai-completions | 200000 | yes | yes | - | - | preserve | - | - | send | 100000 | U |
| openai | o1-2024-12-17 | openai-completions | 200000 | yes | yes | - | - | preserve | - | - | send | 100000 | U |
| openai | o3 | openai-completions | 200000 | yes | yes | - | - | preserve | - | - | send | 100000 | U |
| openai | o3-2025-04-16 | openai-completions | 200000 | yes | yes | - | - | preserve | - | - | send | 100000 | U |
| openai | o3-mini | openai-completions | 200000 | yes | no | - | - | preserve | - | - | send | 100000 | U |
| openai | o3-mini-2025-01-31 | openai-completions | 200000 | yes | no | - | - | preserve | - | - | send | 100000 | U |
| openai | o4-mini | openai-completions | 200000 | yes | yes | - | - | preserve | - | - | send | 100000 | U |
| openai | o4-mini-2025-04-16 | openai-completions | 200000 | yes | yes | - | - | preserve | - | - | send | 100000 | U |
| openai | text-embedding-3-large | openai-completions | 8191 | no | no | - | - | preserve | - | - | send | n/a | U |
| openai | text-embedding-3-small | openai-completions | 8191 | no | no | - | - | preserve | - | - | send | n/a | U |
| qwen | qwen-flash | openai-completions | 997952 | yes | no | qwen | - | preserve | reasoning_content | enable_thinking | send | 32768 | U |
| qwen | qwen-long | openai-completions | 10000000 | no | no | qwen | - | preserve | - | - | send | 16384 | U |
| qwen | qwen-max | openai-completions | 32768 | no | yes | qwen | - | preserve | - | - | send | 16384 | U |
| qwen | qwen-plus | openai-completions | 131072 | no | yes | qwen | - | preserve | - | - | send | 16384 | U |
| qwen | qwen-turbo | openai-completions | 131072 | no | yes | qwen | - | preserve | - | - | send | 16384 | U |
| qwen | qwen-vl-max | openai-completions | 128000 | no | yes | qwen | - | preserve | - | - | send | 8192 | U |
| qwen | qwen-vl-plus | openai-completions | 128000 | no | yes | qwen | - | preserve | - | - | send | 8192 | U |
| qwen | qwen3-235b-a22b | openai-completions | 131072 | yes | no | qwen | - | preserve | reasoning_content | enable_thinking | send | U | U |
| qwen | qwen3-coder-flash | openai-completions | 997952 | yes | yes | qwen | - | preserve | reasoning_content | enable_thinking | send | 65536 | U |
| qwen | qwen3-coder-plus | openai-completions | 997952 | yes | no | qwen | - | preserve | reasoning_content | enable_thinking | send | 65536 | U |
| qwen | qwen3-max | openai-completions | 262144 | yes | yes | qwen | - | preserve | reasoning_content | enable_thinking | send | 32768 | U |
| qwen | qwen3-mini | openai-completions | 131072 | yes | yes | qwen | - | preserve | reasoning_content | enable_thinking | send | 16384 | U |
| qwen | qwen3-plus | openai-completions | 131072 | yes | yes | qwen | - | preserve | reasoning_content | enable_thinking | send | 16384 | U |
| qwen | qwen3.5-flash | openai-completions | 131072 | yes | yes | qwen | - | preserve | reasoning_content | enable_thinking | send | 8192 | U |
| qwen | qwen3.5-max | openai-completions | 1000000 | yes | yes | qwen | - | preserve | reasoning_content | enable_thinking | send | 65536 | U |
| qwen | qwen3.5-plus | openai-completions | 1000000 | yes | yes | qwen | - | preserve | reasoning_content | enable_thinking | send | 65536 | U |
| qwen | qwen3.6-flash | openai-completions | 131072 | yes | yes | qwen | - | preserve | reasoning_content | enable_thinking | send | 8192 | U |
| qwen | qwen3.6-plus | openai-completions | 1000000 | yes | yes | qwen | - | preserve | reasoning_content | enable_thinking | send | 65536 | U |
| volcengine | deepseek-v4-flash-260425 | openai-completions | 1048576 | no | no | volcengine | - | preserve | - | - | omit | U | U |
| volcengine | deepseek-v4-pro-260425 | openai-completions | 1048576 | yes | no | volcengine | - | preserve | reasoning_content | volcengine | omit | U | U |
| volcengine | doubao-lite-128k | openai-completions | 131072 | no | no | volcengine | - | preserve | - | - | omit | 4096 | U |
| volcengine | doubao-pro-128k | openai-completions | 131072 | no | no | volcengine | - | preserve | - | - | omit | 4096 | U |
| volcengine | doubao-pro-256k | openai-completions | 262144 | no | no | volcengine | - | preserve | - | - | omit | 4096 | U |
| volcengine | doubao-seed-2-0-code-preview-260215 | openai-completions | 256000 | yes | yes | volcengine | - | preserve | reasoning_content | volcengine | omit | 128000 | U |
| volcengine | doubao-seed-2-0-lite-260215 | openai-completions | 256000 | yes | yes | volcengine | - | preserve | reasoning_content | volcengine | omit | 128000 | U |
| volcengine | doubao-seed-2-0-lite-260428 | openai-completions | 256000 | yes | yes | volcengine | - | preserve | reasoning_content | volcengine | omit | 128000 | U |
| volcengine | doubao-seed-2-0-mini-260215 | openai-completions | 131072 | no | yes | volcengine | - | preserve | - | - | omit | 16384 | U |
| volcengine | doubao-seed-2-0-pro-260215 | openai-completions | 262144 | yes | yes | volcengine | - | preserve | reasoning_content | volcengine | omit | 16384 | U |
| volcengine | doubao-seed-2-1-pro-260628 | openai-completions | 262144 | yes | yes | volcengine | - | preserve | reasoning_content | volcengine | omit | U | U |
| volcengine | doubao-seed-2-1-turbo-260628 | openai-completions | 262144 | yes | yes | volcengine | - | preserve | reasoning_content | volcengine | omit | U | U |
| volcengine | glm-4-7-251222 | openai-completions | 204800 | yes | no | volcengine | - | preserve | reasoning_content | volcengine | omit | 131072 | U |
| volcengine | kimi-k2-thinking-251104 | openai-completions | 229376 | yes | no | volcengine | - | preserve | reasoning_content | volcengine | omit | 32768 | U |
| zhipu | glm-4-air | openai-completions | 128000 | no | no | zhipu | - | require_tool_call | - | - | omit | U | U |
| zhipu | glm-4-flash | openai-completions | 131072 | no | yes | zhipu | - | require_tool_call | - | - | omit | 4096 | U |
| zhipu | glm-4-plus | openai-completions | 131072 | no | yes | zhipu | - | require_tool_call | - | - | omit | 16384 | U |
| zhipu | glm-4.5-air | openai-completions | 200000 | yes | no | zhipu | - | require_tool_call | reasoning_content | zhipu | omit | U | U |
| zhipu | glm-4.7 | openai-completions | 200000 | yes | no | zhipu | - | require_tool_call | reasoning_content | zhipu | omit | U | U |
| zhipu | glm-4.7-flash | openai-completions | 200000 | yes | no | zhipu | - | require_tool_call | reasoning_content | zhipu | omit | 128000 | U |
| zhipu | glm-5 | openai-completions | 200000 | yes | no | zhipu | - | require_tool_call | reasoning_content | zhipu | omit | 128000 | U |
| zhipu | glm-5-turbo | openai-completions | 205000 | yes | no | zhipu | - | require_tool_call | reasoning_content | zhipu | omit | 131072 | U |
| zhipu | glm-5.1 | openai-completions | 200000 | yes | no | zhipu | - | require_tool_call | reasoning_content | zhipu | omit | 128000 | U |
| zhipu | glm-5.2 | openai-completions | 1000000 | yes | no | zhipu | - | require_tool_call | reasoning_content | zhipu | omit | 131072 | U |
| zhipu | glm-5.3 | openai-completions | 1000000 | yes | no | zhipu | low,high,max (max) | preserve | reasoning_content | zhipu | omit | U | zhipu-glm-5.3 |
| zhipu | glm-5.3-flash | openai-completions | 1000000 | yes | yes | zhipu | low,high,max (max) | preserve | reasoning_content | zhipu | omit | U | zhipu-glm-5.3-flash |
| zhipu | glm-5.3-flashx | openai-completions | 1000000 | yes | yes | zhipu | low,high,max (max) | preserve | reasoning_content | zhipu | omit | U | zhipu-glm-5.3-flash |
| zhipu | glm-5v-turbo | openai-completions | 200000 | yes | yes | zhipu | - | require_tool_call | reasoning_content | zhipu | omit | 128000 | U |
