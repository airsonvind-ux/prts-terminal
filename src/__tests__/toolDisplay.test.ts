// src/__tests__/toolDisplay.test.ts
// toolAction 映射测试：精确表（在册工具）+ 前缀兜底 + 未知兜底

import { describe, it, expect } from "vitest";
import { toolAction } from "../toolDisplay";

describe("toolAction · 精确映射（在册工具）", () => {
  it.each([
    // query_* + search_entities → 查询数据库
    ["query_enemy", "查询数据库"],
    ["query_operator", "查询数据库"],
    ["query_level", "查询数据库"],
    ["query_stage", "查询数据库"],
    ["query_trap", "查询数据库"],
    ["query_token", "查询数据库"],
    ["query_rogue", "查询数据库"],
    ["query_table", "查询数据库"],
    ["search_entities", "查询数据库"],
    // 知识库导航 / 阅读
    ["list_knowledge", "浏览知识库目录"],
    ["read_knowledge", "阅读"],
    ["read_file", "阅读"],
    // 文件域写 / 检索（M2）
    ["write_file", "写入"],
    ["edit", "编辑"],
    ["grep", "搜索"],
    ["list_dir", "浏览"],
    // 命令执行（M3-A）
    ["run_command", "执行命令"],
    // 行动推演
    ["sim_path", "模拟"],
    // Skill 激活
    ["activate_skill", "激活skill"],
    // 时间参照
    ["time", "查看时间"],
    // echo：测试工具，无专门条目 → 走兜底
    ["echo", "处理"],
  ])("toolAction(%s) === %s", (toolName, expected) => {
    expect(toolAction(toolName)).toBe(expected);
  });
});

describe("toolAction · 前缀兜底（未来新增工具自动归类）", () => {
  it.each([
    ["query_foo", "查询数据库"],
    ["read_foo", "阅读"],
    ["write_foo", "写入"],
    ["list_foo", "浏览"],
    ["search_foo", "搜索"],
    ["sim_foo", "模拟"],
    ["run_foo", "执行"],
    // 网络域（M5）：web_ 前缀 → 浏览网页
    ["web_search", "浏览网页"],
    ["web_fetch", "浏览网页"],
    ["web_foo", "浏览网页"],
  ])("toolAction(%s) === %s", (toolName, expected) => {
    expect(toolAction(toolName)).toBe(expected);
  });
});

describe("toolAction · 未知兜底", () => {
  it.each([
    ["some_random_tool", "处理"],
    ["", "处理"],
  ])("toolAction(%j) === %j", (toolName, expected) => {
    expect(toolAction(toolName)).toBe(expected);
  });
});
