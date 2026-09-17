// src/utils.ts
// 共享工具函数 · 不涉及业务逻辑

/** 生成短唯一标识（36 进制时间戳 + 随机后缀） */
export function generateId(): string {
  return Date.now().toString(36) + Math.random().toString(36).slice(2, 8);
}
