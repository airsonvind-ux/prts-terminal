// @vitest-environment jsdom
// degradedLine：载入界面收尾后的"未就绪说明句"（纯函数，固定文案）。
//
// 口径（2026-10-10 定案）：
//   - 全部就绪 / 报告未到 → 空串（不显示、不误报）
//   - 单项未就绪 → 单句（用该组件自身措辞）
//   - 多项未就绪 → 压成一行，影响并列
//   - 原因原文不进载入界面（在日志与用到时的报错里）

import { describe, expect, it } from "vitest";
import { degradedLine } from "./BootSequence";

const OK = { ok: true, note: "" };
const bad = (note: string) => ({ ok: false, note });

function status(over: {
  database?: { ok: boolean; note: string };
  knowledge?: { ok: boolean; note: string };
  memory?: { ok: boolean; note: string };
}) {
  return { database: OK, knowledge: OK, memory: OK, ...over };
}

describe("degradedLine", () => {
  it("全部就绪 → 空串（不加说明行）", () => {
    expect(degradedLine(status({}))).toBe("");
  });

  it("报告未到（null）→ 空串（不误报）", () => {
    expect(degradedLine(null)).toBe("");
  });

  it("单项未就绪 → 单句，用该项自身措辞", () => {
    expect(degradedLine(status({ database: bad("文件不存在") }))).toBe(
      "数据库未就绪：数据查询与推演不可用。"
    );
    expect(degradedLine(status({ knowledge: bad("目录缺失") }))).toBe(
      "知识库未就绪：机制类问答不可用。"
    );
    expect(degradedLine(status({ memory: bad("文件已损坏") }))).toBe(
      "记忆模块未就绪：记忆功能已关闭。"
    );
  });

  it("多项未就绪 → 压成一行，影响并列", () => {
    expect(
      degradedLine(status({ database: bad("x"), knowledge: bad("y") }))
    ).toBe("数据库、知识库未就绪：数据查询与推演、机制类问答均不可用。");
  });

  it("三项全未就绪 → 一行说清", () => {
    expect(
      degradedLine(
        status({ database: bad("x"), knowledge: bad("y"), memory: bad("z") })
      )
    ).toBe("数据库、知识库、记忆模块未就绪：数据查询与推演、机制类问答、记忆均不可用。");
  });
});
