// src/hooks/useUpdaterEvents.ts
// 更新状态监听：启动时拉一次状态，之后只吃 update-status 事件。
//
// 与 useAgentEvents 同级，由 App 调用一次；不直接操作 updater。

import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import type { UpdateStatus } from "../types";
import { useUpdaterStore } from "../stores/useUpdaterStore";

export function useUpdaterEvents(): void {
  useEffect(() => {
    const store = useUpdaterStore.getState();
    const setup = listen<UpdateStatus>("update-status", (event) => {
      store.applyStatus(event.payload);
    });
    void store.init();
    return () => {
      setup.then((unlisten) => unlisten());
    };
  }, []);
}
