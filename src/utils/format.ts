// ============================================================================
// Formatting utilities
// ============================================================================

/**
 * Format a file size into a human-readable string
 * @param bytes number of bytes
 * @returns the formatted string
 */
export function formatSize(bytes: number): string {
  const KB = 1024;
  const MB = KB * 1024;
  const GB = MB * 1024;

  if (bytes >= GB) {
    return `${(bytes / GB).toFixed(2)} GB`;
  } else if (bytes >= MB) {
    return `${(bytes / MB).toFixed(2)} MB`;
  } else if (bytes >= KB) {
    return `${(bytes / KB).toFixed(2)} KB`;
  } else {
    return `${bytes} B`;
  }
}

/**
 * Format a timestamp into a date string
 * @param timestamp Unix timestamp (seconds)
 * @returns the formatted date string
 */
export function formatDate(timestamp: number): string {
  const date = new Date(timestamp * 1000);
  return date.toLocaleString('zh-CN', {
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  });
}

/**
 * Format a duration
 * @param ms milliseconds
 * @returns the formatted string
 */
export function formatDuration(ms: number): string {
  if (ms < 1000) {
    return `${ms} 毫秒`;
  } else if (ms < 60000) {
    return `${(ms / 1000).toFixed(1)} 秒`;
  } else {
    const minutes = Math.floor(ms / 60000);
    const seconds = Math.floor((ms % 60000) / 1000);
    return `${minutes} 分 ${seconds} 秒`;
  }
}

/**
 * Get the color for a risk level
 * @param level risk level (1-5)
 * @returns color class name
 */
export function getRiskLevelColor(level: number): string {
  switch (level) {
    case 1:
      return 'text-green-500';
    case 2:
      return 'text-lime-500';
    case 3:
      return 'text-yellow-500';
    case 4:
      return 'text-orange-500';
    case 5:
      return 'text-red-500';
    default:
      return 'text-gray-500';
  }
}

/**
 * Get the background color for a risk level
 * @param level risk level (1-5)
 * @returns background color class name
 */
export function getRiskLevelBgColor(level: number): string {
  switch (level) {
    case 1:
      return 'bg-green-500/10';
    case 2:
      return 'bg-lime-500/10';
    case 3:
      return 'bg-yellow-500/10';
    case 4:
      return 'bg-orange-500/10';
    case 5:
      return 'bg-red-500/10';
    default:
      return 'bg-gray-500/10';
  }
}

/**
 * Get the text description of a risk level
 * @param level risk level (1-5)
 * @returns risk description
 */
export function getRiskLevelText(level: number): string {
  switch (level) {
    case 1:
      return '安全';
    case 2:
      return '低风险';
    case 3:
      return '中等';
    case 4:
      return '较高';
    case 5:
      return '高风险';
    default:
      return '未知';
  }
}

// Risk level is a stable numeric enum from the backend; the caller uses the language pack to generate the display text.
export function getRiskLevelKey(level: number): string {
  switch (level) {
    case 1: return 'safe';
    case 2: return 'lowRisk';
    case 3: return 'mediumRisk';
    case 4: return 'highRisk';
    case 5: return 'criticalRisk';
    default: return 'unknown';
  }
}
