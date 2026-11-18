/**
 * Data structures for dev caches, other software caches and multi-version leftovers.
 *
 * Fields correspond one-to-one with the Serialize output of the backend `scanner/dev_cache/types.rs` (camelCase).
 * The frontend `src/types/devCache.ts` and that Rust file are the same contract; change one and you must sync the other.
 *
 * Three commands scan three disjoint slices because the UI shows them on two pages
 * (`scanDevCache` / `scanVersionLeftovers` / `scanOtherAppCaches`); they only share the same directory measurement
 * and deletion validation.
 */

/**
 * Risk level of a single item.
 *
 * - `cache`: pure cache, regenerated on next run with no noticeable effect -> allowed to be checked by default
 * - `confirm`: would lose config/data (emulator images, workspace state, local models) -> allowed to be checked but with an explicit warning
 * - `readonly`: display only, **never given a checkbox** (installation caches, system components, etc. whose deletion requires reinstalling or repairing the system)
 */
export type DevRisk = 'cache' | 'confirm' | 'readonly';

/**
 * Link type. Corresponds to `LinkKind` in the backend `scanner/reparse.rs`.
 *
 * `junction` = a `mklink /J` directory junction (the default used by relocation tools like FolderMove),
 * `symlink`  = a `mklink /D` directory symbolic link.
 */
export type LinkKind = 'symlink' | 'junction';

/** A link relationship along a path: which level is a link, where it points, and where it finally lands */
export interface LinkInfo {
  kind: LinkKind;
  /** Which level in the path is a link (usually not the scanned item itself) */
  linkPath: string;
  /** The target recorded by the link */
  target: string;
  /** The real landing point after following all links, i.e. where the space is actually occupied */
  resolved: string;
}

/** A cleanable item (a specific directory) */
export interface DevCacheEntry {
  /** Stable id, used as a key in the UI */
  id: string;
  name: string;
  path: string;
  size: number;
  fileCount: number;
  risk: DevRisk;
  /** A one-line explanation of "what happens after deletion" */
  note: string;
  /** Whether the directory currently exists; if not, it usually means the tool is not installed, not a failure */
  exists: boolean;
  /** Only has a value when the path goes through a junction / symlink */
  link?: LinkInfo;
}

/** A group (classified by toolchain), shown as a collapsible card in the UI */
export interface DevCacheGroup {
  groupId: string;
  groupName: string;
  totalSize: number;
  fileCount: number;
  entries: DevCacheEntry[];
}

/** One version among multi-version directories */
export interface VersionEntry {
  path: string;
  /** Version number parsed from the directory name; if it cannot be parsed, the directory name itself */
  version: string;
  size: number;
  fileCount: number;
  /** The version currently in use: marked in the UI and cannot be checked */
  isCurrent: boolean;
  /** How the current version was determined (registry version / latest version number), shown to avoid presenting a guess as fact */
  currentReason: string;
}

/** A set of same-product multi-version directories (e.g. 5 version directories under Edge's EdgeCore) */
export interface VersionGroup {
  id: string;
  name: string;
  basePath: string;
  totalSize: number;
  /** Total that can be deleted excluding the current version */
  reclaimableSize: number;
  entries: VersionEntry[];
}

export interface DevCacheScanResult {
  groups: DevCacheGroup[];
  /**
   * Total usage of tool caches only. Multi-version directories are **not** included:
   * they come from `VersionLeftoverScanResult` and are shown on the "leftover & cache" page.
   */
  totalSize: number;
  totalFiles: number;
  /** Total of items with risk cache that can be cleaned directly (excluding confirm / readonly) */
  safeSize: number;
  elapsedMs: number;
  /** Number of paths skipped during scanning due to permissions/locks, used to explain "why it is smaller than expected" */
  skipped: number;
}

/**
 * Result of the standalone multi-version leftover scan (same software keeping several version directories side by side).
 *
 * Split from `DevCacheScanResult` because the two live on different pages: tool caches stay in "dev cache cleanup",
 * multi-version directories are shown in "leftover & cache".
 */
export interface VersionLeftoverScanResult {
  groups: VersionGroup[];
  /** Total of every version directory (including the one in use) */
  totalSize: number;
  totalFiles: number;
  /** Total excluding the version currently in use */
  reclaimableSize: number;
  elapsedMs: number;
  /** Version directories skipped while measuring (permissions / in use) */
  skipped: number;
}

/** A single deletion failure item */
export interface DevCacheDeleteFailure {
  path: string;
  /** Failure reason (raw system error text), for troubleshooting only */
  reason: string;
}

export interface DevCacheDeleteResult {
  deleted: number;
  freed: number;
  failed: DevCacheDeleteFailure[];
}
