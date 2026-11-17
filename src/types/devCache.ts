/**
 * 开发空间（工具链缓存 + 多版本残留）的数据结构。
 *
 * 字段与后端 `scanner/dev_cache/types.rs` 的 Serialize 输出一一对应（camelCase）。
 * 前端 `src/types/devCache.ts` 与那份 Rust 文件是同一套契约，改一边就要同步另一边。
 */

/**
 * 单项风险等级。
 *
 * - `cache`：纯缓存，删了下次运行会重新生成，无感 → 默认允许勾选
 * - `confirm`：会丢配置/数据（模拟器镜像、工作区状态、本地模型）→ 允许勾选但要显式提醒
 * - `readonly`：只展示，**永远不给勾选框**（安装缓存、系统组件之类，删了要重装或修系统）
 */
export type DevRisk = 'cache' | 'confirm' | 'readonly';

/**
 * 链接类型。对应后端 `scanner/reparse.rs` 的 `LinkKind`。
 *
 * `junction` = `mklink /J` 目录联接（FolderMove 这类搬迁工具默认用它），
 * `symlink`  = `mklink /D` 目录符号链接。
 */
export type LinkKind = 'symlink' | 'junction';

/** 一条路径上的链接关系：哪一层是链接、指向哪、最终落在哪 */
export interface LinkInfo {
  kind: LinkKind;
  /** 路径里哪一层是链接（通常不是被扫的那一项本身） */
  linkPath: string;
  /** 链接记录的目标 */
  target: string;
  /** 跟随所有链接后的真实落点，也就是空间真正被占用的地方 */
  resolved: string;
}

/** 一个可清理项（一个具体目录） */
export interface DevCacheEntry {
  /** 稳定 id，界面上做 key 用 */
  id: string;
  name: string;
  path: string;
  size: number;
  fileCount: number;
  risk: DevRisk;
  /** 一句话说明「删掉之后会怎样」 */
  note: string;
  /** 目录当前是否存在；不存在通常意味着这个工具没装，不算失败 */
  exists: boolean;
  /** 路径经过 junction / symlink 时才有值 */
  link?: LinkInfo;
}

/** 一个分组（按工具链归类），界面上一层折叠卡片 */
export interface DevCacheGroup {
  groupId: string;
  groupName: string;
  totalSize: number;
  fileCount: number;
  entries: DevCacheEntry[];
}

/** 多版本目录里的一个版本 */
export interface VersionEntry {
  path: string;
  /** 从目录名解析出的版本号，解析不出来就是目录名本身 */
  version: string;
  size: number;
  fileCount: number;
  /** 正在被使用的那一版：界面上标出来并禁止勾选 */
  isCurrent: boolean;
  /** 当前版本是靠什么判定的（注册表版本号 / 最新版本号），展示出来避免把推测当事实 */
  currentReason: string;
}

/** 一组同款多版本目录（如 Edge 的 EdgeCore 下有 5 个版本目录） */
export interface VersionGroup {
  id: string;
  name: string;
  basePath: string;
  totalSize: number;
  /** 除当前版本外可删的合计 */
  reclaimableSize: number;
  entries: VersionEntry[];
}

export interface DevCacheScanResult {
  groups: DevCacheGroup[];
  /** 工具缓存 + 多版本目录的合计：界面上「总计占用」与两个页签之和要对得上 */
  totalSize: number;
  totalFiles: number;
  /** 风险为 cache、可直接清理的合计（不含 confirm / readonly，也不含旧版本目录） */
  safeSize: number;
  versionGroups: VersionGroup[];
  versionReclaimableSize: number;
  elapsedMs: number;
  /** 扫描时因权限/占用跳过的路径数，用于解释"为什么比预期小" */
  skipped: number;
}

/** 单个删除失败项 */
export interface DevCacheDeleteFailure {
  path: string;
  /** 失败原因（系统错误原文），仅用于排查 */
  reason: string;
}

export interface DevCacheDeleteResult {
  deleted: number;
  freed: number;
  failed: DevCacheDeleteFailure[];
}
