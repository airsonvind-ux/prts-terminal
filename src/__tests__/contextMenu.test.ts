// @vitest-environment jsdom
// contextMenu · 统一判定与应用级动作
//
// 覆盖：resolveMenuItems 四组合（文字四件套的出现条件）、字段选中读写、
//       getPageSelection、performCtxAction（复制/剪切/粘贴/全选——剪贴板 mock）。

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  findEditable,
  getFieldSelection,
  getPageSelection,
  hasFieldSelection,
  performCtxAction,
  resolveMenuItems,
} from "../contextMenu";

/** 剪贴板 mock（jsdom 无 clipboard；用 defineProperty 挂上） */
function mockClipboard(initial = "") {
  let clip = initial;
  const clipboard = {
    readText: vi.fn(async () => clip),
    writeText: vi.fn(async (text: string) => {
      clip = text;
    }),
  };
  Object.defineProperty(navigator, "clipboard", {
    value: clipboard,
    configurable: true,
  });
  return {
    clipboard,
    get: () => clip,
    set: (v: string) => {
      clip = v;
    },
  };
}

afterEach(() => {
  vi.restoreAllMocks();
  document.body.replaceChildren();
  Object.defineProperty(navigator, "clipboard", {
    value: undefined,
    configurable: true,
  });
});

describe("contextMenu · 判定", () => {
  it("可编辑域无选中 → 粘贴 / 全选", () => {
    const items = resolveMenuItems({
      editable: true,
      hasFieldSelection: false,
      selectedText: "",
    });
    expect(items?.map((i) => i.id)).toEqual(["paste", "selectAll"]);
  });

  it("可编辑域有选中 → 剪切 / 复制 / 粘贴 / 全选", () => {
    const items = resolveMenuItems({
      editable: true,
      hasFieldSelection: true,
      selectedText: "",
    });
    expect(items?.map((i) => i.id)).toEqual(["cut", "copy", "paste", "selectAll"]);
  });

  it("非编辑域有选中 → 复制", () => {
    const items = resolveMenuItems({
      editable: false,
      hasFieldSelection: false,
      selectedText: "abc",
    });
    expect(items?.map((i) => i.id)).toEqual(["copy"]);
  });

  it("其余 → 无菜单", () => {
    expect(
      resolveMenuItems({ editable: false, hasFieldSelection: false, selectedText: "" })
    ).toBeNull();
  });

  it("findEditable：命中 input/textarea；普通元素与 null 不命中", () => {
    document.body.innerHTML = `<div id="box"><input id="i" /><textarea id="t"></textarea></div>`;
    const input = document.getElementById("i")!;
    const textarea = document.getElementById("t")!;
    const box = document.getElementById("box")!;
    expect(findEditable(input)).toBe(input);
    expect(findEditable(textarea)).toBe(textarea);
    expect(findEditable(box)).toBeNull();
    expect(findEditable(null)).toBeNull();
  });

  it("hasFieldSelection / getFieldSelection：按选区读写", () => {
    document.body.innerHTML = `<textarea></textarea>`;
    const el = document.querySelector("textarea")!;
    el.value = "hello world";
    el.setSelectionRange(0, 5);
    expect(hasFieldSelection(el)).toBe(true);
    expect(getFieldSelection(el)).toBe("hello");
    el.setSelectionRange(5, 5);
    expect(hasFieldSelection(el)).toBe(false);
    expect(getFieldSelection(el)).toBe("");
  });

  it("getPageSelection：trim；空选区返回空串", () => {
    const spy = vi.spyOn(window, "getSelection");
    spy.mockReturnValue({ toString: () => "  hi  " } as unknown as Selection);
    expect(getPageSelection()).toBe("hi");
    spy.mockReturnValue({ toString: () => "   " } as unknown as Selection);
    expect(getPageSelection()).toBe("");
  });
});

describe("contextMenu · 动作", () => {
  let el: HTMLTextAreaElement;

  beforeEach(() => {
    document.body.innerHTML = `<textarea></textarea>`;
    el = document.querySelector("textarea")!;
  });

  it("复制（编辑域）：写入域内选中", async () => {
    const cb = mockClipboard();
    el.value = "hello world";
    el.setSelectionRange(0, 5);
    await performCtxAction("copy", el, "");
    expect(cb.clipboard.writeText).toHaveBeenCalledWith("hello");
  });

  it("复制（非编辑域）：写入页面选中文本", async () => {
    const cb = mockClipboard();
    await performCtxAction("copy", null, "page text");
    expect(cb.clipboard.writeText).toHaveBeenCalledWith("page text");
  });

  it("剪切：复制选中后删除，并触发 input", async () => {
    const cb = mockClipboard();
    el.value = "hello world";
    el.setSelectionRange(0, 6); // "hello "
    const inputSpy = vi.fn();
    el.addEventListener("input", inputSpy);
    await performCtxAction("cut", el, "");
    expect(cb.get()).toBe("hello ");
    expect(el.value).toBe("world");
    expect(inputSpy).toHaveBeenCalledTimes(1);
  });

  it("剪切：剪贴板失败时保持原状（不删除）", async () => {
    Object.defineProperty(navigator, "clipboard", {
      value: {
        readText: vi.fn(async () => ""),
        writeText: vi.fn(async () => {
          throw new Error("denied");
        }),
      },
      configurable: true,
    });
    el.value = "keep me";
    el.setSelectionRange(0, 4);
    await performCtxAction("cut", el, "");
    expect(el.value).toBe("keep me");
  });

  it("粘贴：读剪贴板插入当前选区，光标落在插入内容之后，并触发 input", async () => {
    mockClipboard("PASTED");
    el.value = "ab";
    el.setSelectionRange(1, 1);
    const inputSpy = vi.fn();
    el.addEventListener("input", inputSpy);
    await performCtxAction("paste", el, "");
    expect(el.value).toBe("aPASTEDb");
    expect(el.selectionStart).toBe(7); // "aPASTED" 之后
    expect(el.selectionEnd).toBe(7);
    expect(inputSpy).toHaveBeenCalledTimes(1);
  });

  it("粘贴：剪贴板为空时不动作", async () => {
    mockClipboard("");
    el.value = "ab";
    el.setSelectionRange(1, 1);
    await performCtxAction("paste", el, "");
    expect(el.value).toBe("ab");
  });

  it("全选：选中整个编辑域", async () => {
    el.value = "abcdef";
    await performCtxAction("selectAll", el, "");
    expect(el.selectionStart).toBe(0);
    expect(el.selectionEnd).toBe(6);
  });
});
