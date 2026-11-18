// ============================================================================
// Official download channel config
// Dynamically read channels like cloud drives from GitHub Release's download.json, avoiding hard-coding third-party download links into frontend components.
// ============================================================================

import { LIGHTC_DEFAULT_DOWNLOAD_CONFIG } from '../config/officialLinks';

export interface OfficialDownloadConfig {
  githubReleasesUrl: string;
  netDiskUrl: string;
  bilibiliUrl: string;
  douyinUrl: string;
}

const DOWNLOAD_CONFIG_URL = 'https://github.com/Chunyu33/light-c/releases/latest/download/download.json';

const DEFAULT_DOWNLOAD_CONFIG: OfficialDownloadConfig = {
  ...LIGHTC_DEFAULT_DOWNLOAD_CONFIG,
  // This object's fields are maintained centrally by separate official-link constants, avoiding the settings page hard-coding them again.
  githubReleasesUrl: LIGHTC_DEFAULT_DOWNLOAD_CONFIG.githubReleasesUrl ?? 'https://github.com/Chunyu33/light-c/releases',
};

let cachedConfigPromise: Promise<OfficialDownloadConfig> | null = null;

function isSafeHttpsUrl(url: unknown): url is string {
  return typeof url === 'string' && url.startsWith('https://');
}

function mergeDownloadConfig(remoteConfig: unknown): OfficialDownloadConfig {
  if (!remoteConfig || typeof remoteConfig !== 'object') {
    return DEFAULT_DOWNLOAD_CONFIG;
  }

  const config = remoteConfig as Partial<Record<keyof OfficialDownloadConfig, unknown>>;

  return {
    // Remote fields must be https, preventing abnormal JSON from injecting non-official or local protocols.
    githubReleasesUrl: isSafeHttpsUrl(config.githubReleasesUrl)
      ? config.githubReleasesUrl
      : DEFAULT_DOWNLOAD_CONFIG.githubReleasesUrl,
    bilibiliUrl: isSafeHttpsUrl(config.bilibiliUrl)
      ? config.bilibiliUrl
      : DEFAULT_DOWNLOAD_CONFIG.bilibiliUrl,
    douyinUrl: isSafeHttpsUrl(config.douyinUrl)
      ? config.douyinUrl
      : DEFAULT_DOWNLOAD_CONFIG.douyinUrl,
    // When a remote field is missing, fall back to the author's cloud drive, otherwise the settings page would hide the entry entirely due to conditional rendering.
    netDiskUrl: isSafeHttpsUrl(config.netDiskUrl)
      ? config.netDiskUrl
      : DEFAULT_DOWNLOAD_CONFIG.netDiskUrl,
  };
}

export async function getOfficialDownloadConfig(): Promise<OfficialDownloadConfig> {
  if (!cachedConfigPromise) {
    cachedConfigPromise = fetch(DOWNLOAD_CONFIG_URL, { cache: 'no-store' })
      .then((response) => {
        if (!response.ok) {
          throw new Error(`download.json 请求失败: ${response.status}`);
        }
        return response.json();
      })
      .then(mergeDownloadConfig)
      .catch((error) => {
        console.warn('读取官方下载配置失败，已降级到内置官方渠道:', error);
        return DEFAULT_DOWNLOAD_CONFIG;
      });
  }

  return cachedConfigPromise;
}
