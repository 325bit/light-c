// ============================================================================
// Author's official channel links
// The remote download.json only updates these entries; when the remote is unavailable, always use the safe fallbacks here.
// ============================================================================

export interface OfficialProjectLinks {
  /** Project display name. */
  name: string;
  /** Author's cloud-drive download entry. */
  quarkUrl: string;
  /** GitHub Releases entry; some projects may not provide one yet. */
  githubReleasesUrl?: string;
}

/** Read the official website uniformly from config to avoid duplicating hardcoded values in the settings page and keep entries consistent. */
export const LIGHTC_OFFICIAL_WEBSITE_URL = 'https://lightc.app/';

/** Project repository homepage. Shared by the sidebar footer and the About page to avoid writing the address in two places. */
export const LIGHTC_GITHUB_URL = 'https://github.com/Chunyu33/light-c';

/** English changelog (CHANGELOG.md in the repository). Used by the docs entry in the sidebar footer. */
export const LIGHTC_CHANGELOG_URL = 'https://github.com/Chunyu33/light-c/blob/main/CHANGELOG.md';

/** Author's personal homepage. */
export const AUTHOR_HOMEPAGE_URL = 'https://www.evan666.cc/';

/** Download entries for projects maintained by the author; adding a project later only requires an entry here. */
export const OFFICIAL_PROJECT_LINKS: Record<string, OfficialProjectLinks> = {
  lightc: {
    name: 'LightC',
    quarkUrl: 'https://pan.quark.cn/s/bce8f722bf33',
    githubReleasesUrl: 'https://github.com/Chunyu33/light-c/releases',
  },
  viap: {
    name: 'Viap',
    quarkUrl: 'https://pan.quark.cn/s/4761ee4ba698',
    githubReleasesUrl: 'https://github.com/chunyu33/viap/releases',
  },
  binlockx: {
    name: 'BinlockX',
    quarkUrl: 'https://pan.quark.cn/s/4243a5142b29',
    githubReleasesUrl: 'https://github.com/chunyu33/binlockx/releases',
  },
  duoduomao: {
    name: '躲躲猫',
    quarkUrl: 'https://pan.quark.cn/s/3bdd5d36b71b',
  },
};

/** The author's own platform entries, used as fixed fallbacks when remote channel config is missing. */
export const OFFICIAL_PLATFORM_LINKS = {
  bilibili: 'https://space.bilibili.com/387797235',
  douyin: 'https://www.douyin.com/search/Evan%E7%9A%84%E5%83%8F%E7%B4%A0%E7%A9%BA%E9%97%B4',
} as const;

/** Default LightC download config shared by SecuritySettings and the update prompt. */
export const LIGHTC_DEFAULT_DOWNLOAD_CONFIG = {
  githubReleasesUrl: OFFICIAL_PROJECT_LINKS.lightc.githubReleasesUrl,
  netDiskUrl: OFFICIAL_PROJECT_LINKS.lightc.quarkUrl,
  bilibiliUrl: OFFICIAL_PLATFORM_LINKS.bilibili,
  douyinUrl: OFFICIAL_PLATFORM_LINKS.douyin,
} as const;
