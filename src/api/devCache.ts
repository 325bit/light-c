/**
 * 开发空间清理的调用层。
 *
 * 只做 invoke 转发，字段与 `src/types/devCache.ts` 对应。
 */
import { invoke } from '@tauri-apps/api/core';
import type { DevCacheDeleteResult, DevCacheScanResult } from '../types/devCache';

/**
 * 扫描开发工具链缓存与多版本残留。
 *
 * 后端内部用 rayon 并行遍历，一次调用就返回全部分组 —— 没有进度事件，
 * 所以界面上要用"扫描中"的骨架屏而不是进度条（进度条需要一个总量，这里拿不到）。
 */
export function scanDevCache(): Promise<DevCacheScanResult> {
  return invoke('scan_dev_cache');
}

/**
 * 删除选中的目录，返回成功数、释放量与失败明细。
 *
 * 前端传来的路径**不可信**：后端会重新校验归属（必须精确等于某个目标目录，
 * 或位于版本根目录之下的版本号目录），只读项与当前正在使用的版本一律拒绝。
 *
 * 返回值必须看 `failed`：IDE / 构建进程开着时文件常被占用，删除会部分失败，
 * 只看 `deleted` 会让界面显示"已删完"而实际占用没降。
 */
export function deleteDevCachePaths(paths: string[]): Promise<DevCacheDeleteResult> {
  return invoke('delete_dev_cache_paths', { paths });
}
