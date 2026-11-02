// 拖拽仲裁：对话区与安装区都收到同一个 webview 级 drop，顺序不定。
// 这里只测仲裁器本身的判定，不碰 Tauri。

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  finishPluginDrop,
  notePluginDragOver,
  pluginDropTakesPrecedence,
} from "./pluginDropArbiter";

let clock = 0;
let nowSpy: ReturnType<typeof vi.spyOn>;

beforeEach(() => {
  clock = 1_000;
  nowSpy = vi.spyOn(Date, "now").mockImplementation(() => clock);
  finishPluginDrop();
});

afterEach(() => {
  nowSpy.mockRestore();
  finishPluginDrop();
});

describe("pluginDropArbiter", () => {
  it("最后一次 dragover 命中安装区，drop 就归安装区", () => {
    notePluginDragOver(true);
    expect(pluginDropTakesPrecedence()).toBe(true);
  });

  it("从安装区拖出来再松手，不归安装区（命中会被后面的 dragover 覆盖）", () => {
    notePluginDragOver(true);
    clock += 200;
    notePluginDragOver(false);
    expect(pluginDropTakesPrecedence()).toBe(false);
  });

  it("drop 之后立刻清掉，下一次投放不会被误让路", () => {
    notePluginDragOver(true);
    expect(pluginDropTakesPrecedence()).toBe(true);
    finishPluginDrop();
    clock += 10;
    expect(pluginDropTakesPrecedence()).toBe(false);
  });

  it("读不是消费：两个监听器先后读，拿到的是同一个结果", () => {
    notePluginDragOver(true);
    expect(pluginDropTakesPrecedence()).toBe(true);
    expect(pluginDropTakesPrecedence()).toBe(true);
  });

  it("陈旧的命中会过期（收不到 leave / dragend 时的兜底）", () => {
    notePluginDragOver(true);
    clock += 2_000;
    expect(pluginDropTakesPrecedence()).toBe(false);
  });

  it("没收到过 dragover 时不认领", () => {
    expect(pluginDropTakesPrecedence()).toBe(false);
  });
});
