// useUpdaterStore · 应用内更新状态
//
// 职责：把后端 updater_status / update-status 事件收敛成一份前端状态，
// 供 Action Dock 与关于页共用。更新是可选任务，不与工具审批状态机合并。
//
// 边界：
// - 检查/下载/安装都只 invoke 后端命令；状态以后端事件为准；
// - 安装后的更新日志由 updater_take_post_install_notice 取一次，读完即删；
// - 安装阻塞原因（活跃对话 / MAA）由 updater_install_blockers 实时查，
//   前端只负责禁用与解释，后端仍会复检。

import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import type {
  PostInstallNotice,
  UpdateStatus,
  UpdaterInstallBlockers,
} from "../types";

type UpdaterAction = "check" | "download" | "install" | null;

interface UpdaterStore {
  status: UpdateStatus | null;
  notice: PostInstallNotice | null;
  blockers: UpdaterInstallBlockers | null;
  action: UpdaterAction;
  actionError: string | null;
  collapsed: boolean;
  initialized: boolean;
  /** 用户主动关闭的可用/已下载版本；同一版本不再自动弹出 */
  dismissedVersion: string | null;
  /** 用户主动关闭过错误卡；手动检查会重新打开 */
  dismissedError: boolean;

  init: () => Promise<void>;
  applyStatus: (status: UpdateStatus) => void;
  check: () => Promise<void>;
  download: () => Promise<void>;
  install: () => Promise<void>;
  refreshBlockers: () => Promise<void>;
  dismissNotice: () => void;
  dismiss: () => void;
  setCollapsed: (collapsed: boolean) => void;
}

function errorText(error: unknown): string {
  return typeof error === "string" ? error : String(error);
}

export const useUpdaterStore = create<UpdaterStore>((set, get) => ({
  status: null,
  notice: null,
  blockers: null,
  action: null,
  actionError: null,
  collapsed: false,
  initialized: false,
  dismissedVersion: null,
  dismissedError: false,

  init: async () => {
    if (get().initialized) return;
    set({ initialized: true });
    try {
      const status = await invoke<UpdateStatus>("updater_status");
      if (status) set({ status });
    } catch (error) {
      set({ actionError: errorText(error) });
    }
    try {
      const notice = await invoke<PostInstallNotice | null>(
        "updater_take_post_install_notice"
      );
      if (notice) set({ notice });
    } catch {
      // 旧后端没有该命令时静默；不影响更新状态。
    }
    try {
      const blockers = await invoke<UpdaterInstallBlockers>(
        "updater_install_blockers"
      );
      if (blockers) set({ blockers });
    } catch {
      // 阻塞检查失败不阻塞更新状态展示。
    }
  },

  applyStatus: (status) => {
    const dismissedVersion = get().dismissedVersion;
    const availableVersion = status.available?.version ?? null;
    const keepDismissed =
      (status.phase === "available" || status.phase === "downloaded") &&
      availableVersion !== null &&
      availableVersion === dismissedVersion;
    set({
      status,
      actionError: null,
      // 新版本出现时重新展示；同版本仍尊重用户关闭
      dismissedVersion: keepDismissed ? dismissedVersion : null,
    });
    if (status.phase === "downloaded") {
      void get().refreshBlockers();
    }
  },

  check: async () => {
    if (get().action !== null) return;
    set({
      action: "check",
      actionError: null,
      dismissedVersion: null,
      dismissedError: false,
    });
    try {
      const status = await invoke<UpdateStatus>("updater_check");
      get().applyStatus(status);
    } catch (error) {
      set({ actionError: errorText(error) });
    } finally {
      set({ action: null });
    }
  },

  download: async () => {
    if (get().action !== null) return;
    const version = get().status?.available?.version;
    if (!version) return;
    set({
      action: "download",
      actionError: null,
      collapsed: false,
      dismissedVersion: null,
      dismissedError: false,
    });
    try {
      const status = await invoke<UpdateStatus>("updater_download", {
        version,
      });
      get().applyStatus(status);
    } catch (error) {
      set({ actionError: errorText(error) });
    } finally {
      set({ action: null });
    }
  },

  install: async () => {
    if (get().action !== null) return;
    set({
      action: "install",
      actionError: null,
      dismissedVersion: null,
      dismissedError: false,
    });
    try {
      await invoke("updater_install");
    } catch (error) {
      set({ actionError: errorText(error) });
    } finally {
      set({ action: null });
    }
  },

  refreshBlockers: async () => {
    try {
      const blockers = await invoke<UpdaterInstallBlockers>(
        "updater_install_blockers"
      );
      set({ blockers });
    } catch {
      // 阻塞检查失败不阻塞主流程；后端安装时仍会复检。
    }
  },

  dismissNotice: () => set({ notice: null }),

  dismiss: () => {
    const status = get().status;
    if (!status) return;
    if (status.phase === "error") {
      set({ dismissedError: true });
      return;
    }
    if (status.phase === "available" || status.phase === "downloaded") {
      set({ dismissedVersion: status.available?.version ?? null });
    }
  },

  setCollapsed: (collapsed) => set({ collapsed }),
}));
