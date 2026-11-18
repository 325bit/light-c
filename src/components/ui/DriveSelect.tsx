import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { getLocalDrives, type LocalDriveInfo } from '../../api/commands';
import { formatSize } from '../../utils/format';
import { Select, type SelectOption } from './Select';
import i18n from '../../i18n';

export function normalizeDriveLetter(value?: string | null): string {
  const letter = value?.match(/[a-z]/i)?.[0]?.toUpperCase() ?? 'C';
  return `${letter}:`;
}

export function driveDisplayName(driveLetter: string): string {
  return i18n.t('driveLabel', { ns: 'common', drive: normalizeDriveLetter(driveLetter).replace(':', '') });
}

export function useLocalDrives() {
  const [drives, setDrives] = useState<LocalDriveInfo[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  /** Don't setState after unmount, to avoid warnings when a refresh request comes back. */
  const mountedRef = useRef(true);

  /**
   * Reads the disk list (including each drive's free space).
   * Returns whether it succeeded — the caller uses it to decide whether to toast "Updated" or "Update failed".
   */
  const loadDrives = useCallback(async (): Promise<boolean> => {
    setLoading(true);
    try {
      const result = await getLocalDrives();
      if (!mountedRef.current) return true;
      setDrives(result);
      setError(null);
      return true;
    } catch (err) {
      if (!mountedRef.current) return false;
      setError(String(err));
      return false;
    } finally {
      if (mountedRef.current) setLoading(false);
    }
  }, []);

  useEffect(() => {
    // ⚠️ "Setting true" must be in the same effect as "setting false".
    // Previously false was set only in cleanup; in React Strict Mode the effect does mount → unmount → mount,
    // and on the second mount mountedRef was already false, so loadDrives returned early on entry,
    // and setLoading(false) was never reached — the symptom was the top bar loading forever and controls stuck disabled.
    mountedRef.current = true;
    void loadDrives();
    return () => {
      mountedRef.current = false;
    };
  }, [loadDrives]);

  return { drives, error, loading, refresh: loadDrives };
}

export function defaultDriveLetter(drives: LocalDriveInfo[]): string {
  const drive =
    drives.find((item) => item.is_system) ??
    drives.find((item) => item.drive_letter === 'C:') ??
    drives[0];
  return drive?.drive_letter ?? 'C:';
}

export function driveOptionTitle(drive: LocalDriveInfo): string {
  const parts = [drive.drive_letter];
  if (drive.volume_name) parts.push(drive.volume_name);
  if (drive.file_system) parts.push(drive.file_system);
  parts.push(i18n.t('driveAvailable', { ns: 'common', size: formatSize(drive.free_space) }));
  parts.push(i18n.t('driveTotal', { ns: 'common', size: formatSize(drive.total_space) }));
  if (drive.is_system) parts.push(i18n.t('systemDrive', { ns: 'common' }));
  if (!drive.is_ntfs) parts.push(i18n.t('nonNtfs', { ns: 'common' }));
  return parts.join(' · ');
}

/**
 * Short label for the drive dropdown item.
 * ⚠️ Must be exported for the top bar to reuse — previously the top bar built its own `${letter} ${volume}`,
 * missing the ` · ` in the middle, so the same dropdown looked different in the settings page and the top bar.
 */
export function driveOptionLabel(drive: LocalDriveInfo): string {
  // The dropdown trigger has limited space, so show only the drive letter and volume label; put capacity, file system, and full info in title.
  return drive.volume_name ? `${drive.drive_letter} · ${drive.volume_name}` : drive.drive_letter;
}

interface DriveSelectProps {
  value: string;
  drives: LocalDriveInfo[];
  onChange: (driveLetter: string) => void;
  disabled?: boolean;
  widthClass?: string;
}

export function DriveSelect({
  value,
  drives,
  onChange,
  disabled = false,
  // Unify the drive-selector width to avoid visual jumps in the title areas of different modules in card mode.
  widthClass = 'w-28',
}: DriveSelectProps) {
  const options = useMemo<SelectOption[]>(
    () => {
      const driveOptions = drives.map((drive) => ({
        value: drive.drive_letter,
        label: driveOptionLabel(drive),
        title: driveOptionTitle(drive),
      }));
      return driveOptions.length > 0
        ? driveOptions
        : [{ value: 'C:', label: 'C:', title: `C: · ${i18n.t('defaultSystemDrive', { ns: 'common' })}` }];
    },
    [drives],
  );

  return (
    <Select
      value={normalizeDriveLetter(value)}
      options={options}
      onChange={(driveLetter) => onChange(normalizeDriveLetter(driveLetter))}
      widthClass={widthClass}
      size="sm"
      disabled={disabled}
    />
  );
}
