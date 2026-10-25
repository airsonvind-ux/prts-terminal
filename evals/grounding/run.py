#!/usr/bin/env python3
"""PRTS 接地评测：用录制的会话日志检查工具调用与证据链。

这不是模型 benchmark。它只读 PRTS 已落盘的会话 JSONL，检查：
- 需要查的问题是否调用了要求的工具；
- 工具结果里是否出现关键证据词；
- 最终回答是否引用/包含这些证据；
- 压缩摘要是否保留了关键证据锚点。

默认不联网、不调用模型、不需要 API Key。
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

DEFAULT_CASES = Path(__file__).with_name("cases.jsonl")
CASE_REQUIRED = ("id", "question")
CASE_LISTS = (
    "required_tools",
    "evidence_terms",
    "answer_terms",
    "compression_terms",
    "forbidden_answer_terms",
)


def fail(message: str) -> "NoReturn":
    print(f"error: {message}", file=sys.stderr)
    raise SystemExit(2)


def load_cases(path: Path) -> list[dict[str, Any]]:
    if not path.is_file():
        fail(f"case file not found: {path}")
    cases: list[dict[str, Any]] = []
    seen: set[str] = set()
    for lineno, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        line = raw.strip()
        if not line:
            continue
        try:
            case = json.loads(line)
        except json.JSONDecodeError as exc:
            fail(f"{path}:{lineno}: invalid JSON: {exc}")
        if not isinstance(case, dict):
            fail(f"{path}:{lineno}: case must be a JSON object")
        case_id = case.get("id")
        if not isinstance(case_id, str) or not case_id.strip():
            fail(f"{path}:{lineno}: id must be a non-empty string")
        if case_id in seen:
            fail(f"{path}:{lineno}: duplicate id: {case_id}")
        seen.add(case_id)
        for field in CASE_REQUIRED:
            value = case.get(field)
            if not isinstance(value, str) or not value.strip():
                fail(f"{path}:{lineno}: {field} must be a non-empty string")
        for field in CASE_LISTS:
            value = case.get(field, [])
            if not isinstance(value, list) or any(
                not isinstance(item, str) or not item for item in value
            ):
                fail(f"{path}:{lineno}: {field} must be a string array")
        cases.append(case)
    if not cases:
        fail(f"{path}: no cases")
    return cases


def load_turns(path: Path) -> list[list[dict[str, Any]]]:
    """读取 PRTS 会话 jsonl：首行 header，后续每行是一轮的 event 数组。"""
    if not path.is_file():
        fail(f"session file not found: {path}")
    turns: list[list[dict[str, Any]]] = []
    for lineno, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        line = raw.strip()
        if not line:
            continue
        try:
            value = json.loads(line)
        except json.JSONDecodeError as exc:
            fail(f"{path}:{lineno}: invalid JSON: {exc}")
        if isinstance(value, list):
            if not all(isinstance(event, dict) for event in value):
                fail(f"{path}:{lineno}: turn events must be JSON objects")
            turns.append(value)
        elif isinstance(value, dict):
            # header 或单事件行：header 跳过，单事件按一轮处理。
            if value.get("type") == "header" or value.get("format") == "prts-conversation-jsonl":
                continue
            turns.append([value])
        else:
            fail(f"{path}:{lineno}: expected object or array")
    if not turns:
        fail(f"{path}: no turn events")
    return turns


def text_of(event: dict[str, Any], key: str = "content") -> str:
    value = event.get(key)
    return value if isinstance(value, str) else ""


def summarize_turn(events: list[dict[str, Any]]) -> dict[str, Any]:
    tool_calls: list[dict[str, Any]] = []
    tool_results: list[dict[str, Any]] = []
    user_messages: list[dict[str, Any]] = []
    assistant_messages: list[dict[str, Any]] = []
    assistant_chunks: list[dict[str, Any]] = []
    compaction_summaries: list[str] = []

    for index, event in enumerate(events):
        kind = event.get("type")
        if kind == "tool/call":
            tool_calls.append(
                {
                    "index": index,
                    "name": event.get("toolName", ""),
                    "params": event.get("params", ""),
                }
            )
        elif kind == "tool/result":
            tool_results.append(
                {
                    "index": index,
                    "name": event.get("toolName", ""),
                    "summary": text_of(event, "summary"),
                    "ok": event.get("ok"),
                }
            )
        elif kind == "user/message":
            content = text_of(event)
            user_messages.append({"index": index, "content": content})
            if content.startswith("[对话压缩摘要"):
                compaction_summaries.append(content)
        elif kind == "assistant/message":
            content = text_of(event)
            if content:
                assistant_messages.append({"index": index, "content": content})
        elif kind == "assistant/chunk" and event.get("kind") == "text":
            content = text_of(event)
            if content:
                assistant_chunks.append({"index": index, "content": content})

    if assistant_messages:
        final = assistant_messages[-1]
    elif assistant_chunks:
        final = {
            "index": assistant_chunks[-1]["index"],
            "content": "".join(chunk["content"] for chunk in assistant_chunks),
        }
    else:
        final = {"index": None, "content": ""}

    return {
        "tool_calls": tool_calls,
        "tool_results": tool_results,
        "user_messages": user_messages,
        "final_answer": final,
        "compaction_summaries": compaction_summaries,
    }


def select_turn(
    turns: list[list[dict[str, Any]]],
    case: dict[str, Any],
    turn_arg: str | None,
) -> tuple[int, list[dict[str, Any]]]:
    if turn_arg:
        if turn_arg == "last":
            return len(turns), turns[-1]
        try:
            number = int(turn_arg)
        except ValueError:
            fail(f"invalid --turn: {turn_arg}")
        if number < 1 or number > len(turns):
            fail(f"--turn out of range: {number} (1..{len(turns)})")
        return number, turns[number - 1]

    # 未显式指定时，先按 case 的问题匹配轮次，再退回最后一轮。
    question = case["question"]
    matches = [
        index
        for index, events in enumerate(turns, 1)
        if any(
            event.get("type") == "user/message" and question in text_of(event)
            for event in events
        )
    ]
    if matches:
        # 同一问题可能重复出现，默认取最后一次。
        return matches[-1], turns[matches[-1] - 1]
    fail(f"question not found in session: {question!r}; use --turn to select a turn")


def evaluate(case: dict[str, Any], summary: dict[str, Any]) -> list[str]:
    failures: list[str] = []
    required_tools = case.get("required_tools", [])
    evidence_terms = case.get("evidence_terms", [])
    answer_terms = case.get("answer_terms", [])
    compression_terms = case.get("compression_terms", [])
    forbidden_terms = case.get("forbidden_answer_terms", [])

    called = {call["name"] for call in summary["tool_calls"]}
    missing_tools = [name for name in required_tools if name not in called]
    if missing_tools:
        failures.append(f"missing tools: {', '.join(missing_tools)}")

    if required_tools and summary["final_answer"]["index"] is not None:
        required_indices = [
            call["index"]
            for call in summary["tool_calls"]
            if call["name"] in required_tools
        ]
        if required_indices:
            first_tool = min(required_indices)
            if summary["final_answer"]["index"] < first_tool:
                failures.append("final answer appears before the required tool call")

    tool_text = "\n".join(
        result["summary"] for result in summary["tool_results"] if result["summary"]
    )
    missing_evidence = [term for term in evidence_terms if term not in tool_text]
    if missing_evidence:
        failures.append(f"evidence not found in tool results: {', '.join(missing_evidence)}")

    answer = summary["final_answer"]["content"]
    missing_answer_terms = [term for term in answer_terms if term not in answer]
    if missing_answer_terms:
        failures.append(f"answer missing terms: {', '.join(missing_answer_terms)}")

    found_forbidden = [term for term in forbidden_terms if term in answer]
    if found_forbidden:
        failures.append(f"answer contains forbidden terms: {', '.join(found_forbidden)}")

    if compression_terms:
        if not summary["compaction_summaries"]:
            failures.append("no compaction summary found")
        else:
            compaction_text = "\n".join(summary["compaction_summaries"])
            missing_compaction = [
                term for term in compression_terms if term not in compaction_text
            ]
            if missing_compaction:
                failures.append(
                    "compaction summary missing terms: "
                    + ", ".join(missing_compaction)
                )

    return failures


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Evaluate recorded PRTS sessions for grounding contracts."
    )
    parser.add_argument(
        "--cases",
        type=Path,
        default=DEFAULT_CASES,
        help=f"case JSONL path (default: {DEFAULT_CASES})",
    )
    parser.add_argument("--session", type=Path, help="PRTS session JSONL file")
    parser.add_argument("--case", help="only evaluate the case with this id")
    parser.add_argument(
        "--turn",
        help="turn number (1-based) or 'last'; default: match by question, then last",
    )
    parser.add_argument("--check", action="store_true", help="validate cases only")
    parser.add_argument("--list", action="store_true", help="list cases")
    args = parser.parse_args()

    cases = load_cases(args.cases)
    if args.case:
        cases = [case for case in cases if case["id"] == args.case]
        if not cases:
            fail(f"case not found: {args.case}")

    if args.list:
        for case in cases:
            print(f"{case['id']}\t{case['question']}")
        return 0

    if args.check:
        print(f"cases: {len(cases)} valid")
        return 0

    if not args.session:
        parser.print_help()
        return 2

    turns = load_turns(args.session)
    failed = 0
    for case in cases:
        number, events = select_turn(turns, case, args.turn)
        summary = summarize_turn(events)
        failures = evaluate(case, summary)
        if failures:
            failed += 1
            print(f"[FAIL] {case['id']} (turn {number})")
            for reason in failures:
                print(f"  - {reason}")
        else:
            print(f"[PASS] {case['id']} (turn {number})")

    print(f"\n{len(cases) - failed}/{len(cases)} passed")
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
