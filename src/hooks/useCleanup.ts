// ============================================================================
// Cleanup feature hook
// Manages the state logic for scanning and deletion
// ============================================================================

import { useState, useCallback, useEffect } from 'react';
import type { ScanResult, DeleteResult, AppStatus, FileInfo } from '../types';
import { scanJunkFiles, deleteFiles, getDiskInfo } from '../api/commands';
import type { DiskInfo } from '../types';
import { errorText } from '../api/errors';

/** Return type of the hook */
interface UseCleanupReturn {
  /** App state */
  status: AppStatus;
  /** Scan result */
  scanResult: ScanResult | null;
  /** Deletion result */
  deleteResult: DeleteResult | null;
  /** Disk info */
  diskInfo: DiskInfo | null;
  /** Selected file paths */
  selectedPaths: Set<string>;
  /** Error message */
  error: string | null;
  /** Run the scan */
  startScan: () => Promise<void>;
  /** Run the deletion */
  startDelete: () => Promise<void>;
  /** Toggle a file's selected state */
  toggleFileSelection: (path: string) => void;
  /** Toggle select-all for a category */
  toggleCategorySelection: (files: FileInfo[], selected: boolean) => void;
  /** Select all / deselect all */
  toggleAllSelection: (selected: boolean) => void;
  /** Refresh disk info */
  refreshDiskInfo: () => Promise<void>;
  /** Clear the error */
  clearError: () => void;
  /** Clear the deletion result */
  clearDeleteResult: () => void;
  /** Reset state */
  reset: () => void;
}

/**
 * Cleanup feature hook
 * Encapsulates all state-management logic for scanning and deletion
 */
export function useCleanup(): UseCleanupReturn {
  // State definitions
  const [status, setStatus] = useState<AppStatus>('idle');
  const [scanResult, setScanResult] = useState<ScanResult | null>(null);
  const [deleteResult, setDeleteResult] = useState<DeleteResult | null>(null);
  const [diskInfo, setDiskInfo] = useState<DiskInfo | null>(null);
  const [selectedPaths, setSelectedPaths] = useState<Set<string>>(new Set());
  const [error, setError] = useState<string | null>(null);

  // Refresh disk info
  const refreshDiskInfo = useCallback(async () => {
    try {
      const info = await getDiskInfo();
      setDiskInfo(info);
    } catch (err) {
      console.error('获取磁盘信息失败:', err);
      setError(`获取磁盘信息失败: ${errorText(err)}`);
    }
  }, []);

  // Fetch disk info automatically at app startup
  useEffect(() => {
    refreshDiskInfo();
  }, [refreshDiskInfo]);

  // Run the scan
  const startScan = useCallback(async () => {
    setStatus('scanning');
    setError(null);
    setScanResult(null);
    setDeleteResult(null);
    setSelectedPaths(new Set());

    try {
      // Refresh disk info first
      await refreshDiskInfo();
      
      // Run the scan
      const result = await scanJunkFiles();
      setScanResult(result);
      
      // Select all files with risk level <= 2 by default
      const defaultSelected = new Set<string>();
      result.categories.forEach((category) => {
        if (category.risk_level <= 2) {
          category.files.forEach((file) => {
            defaultSelected.add(file.path);
          });
        }
      });
      setSelectedPaths(defaultSelected);
    } catch (err) {
      console.error('扫描失败:', err);
      setError(`扫描失败: ${errorText(err)}`);
    } finally {
      setStatus('idle');
    }
  }, [refreshDiskInfo]);

  // Run the deletion
  const startDelete = useCallback(async () => {
    if (selectedPaths.size === 0) {
      setError('请先选择要删除的文件');
      return;
    }

    setStatus('deleting');
    setError(null);
    setDeleteResult(null);

    try {
      const paths = Array.from(selectedPaths);
      const result = await deleteFiles(paths);
      setDeleteResult(result);
      
      // After a successful deletion, remove the deleted files from the scan result
      if (scanResult && result.success_count > 0) {
        const deletedPaths = new Set(
          paths.filter((p) => !result.failed_files.some((f) => f.path === p))
        );
        
        const updatedCategories = scanResult.categories.map((category) => {
          const remainingFiles = category.files.filter((f) => !deletedPaths.has(f.path));
          return {
            ...category,
            files: remainingFiles,
            file_count: remainingFiles.length,
            total_size: remainingFiles.reduce((sum, f) => sum + f.size, 0),
          };
        });

        setScanResult({
          ...scanResult,
          categories: updatedCategories,
          total_file_count: updatedCategories.reduce((acc, c) => acc + c.file_count, 0),
          total_size: updatedCategories.reduce((acc, c) => acc + c.total_size, 0),
        });

        // Clear the selected state of deleted files
        setSelectedPaths((prev) => {
          const newSet = new Set(prev);
          deletedPaths.forEach((p) => newSet.delete(p));
          return newSet;
        });
      }

      // Refresh disk info (placed before finally to ensure it runs)
      await refreshDiskInfo();
    } catch (err) {
      console.error('删除失败:', err);
      setError(`删除失败: ${errorText(err)}`);
    } finally {
      setStatus('idle');
    }
  }, [selectedPaths, scanResult, refreshDiskInfo]);

  // Toggle a single file's selected state
  const toggleFileSelection = useCallback((path: string) => {
    setSelectedPaths((prev) => {
      const newSet = new Set(prev);
      if (newSet.has(path)) {
        newSet.delete(path);
      } else {
        newSet.add(path);
      }
      return newSet;
    });
  }, []);

  // Toggle select-all for a category
  const toggleCategorySelection = useCallback((files: FileInfo[], selected: boolean) => {
    setSelectedPaths((prev) => {
      const newSet = new Set(prev);
      files.forEach((file) => {
        if (selected) {
          newSet.add(file.path);
        } else {
          newSet.delete(file.path);
        }
      });
      return newSet;
    });
  }, []);

  // Select all / deselect all
  const toggleAllSelection = useCallback(
    (selected: boolean) => {
      if (!scanResult) return;

      setSelectedPaths(() => {
        if (selected) {
          const allPaths = new Set<string>();
          scanResult.categories.forEach((category) => {
            category.files.forEach((file) => {
              allPaths.add(file.path);
            });
          });
          return allPaths;
        } else {
          return new Set();
        }
      });
    },
    [scanResult]
  );

  // Clear the error
  const clearError = useCallback(() => {
    setError(null);
  }, []);

  // Clear the deletion result
  const clearDeleteResult = useCallback(() => {
    setDeleteResult(null);
  }, []);

  // Reset state
  const reset = useCallback(() => {
    setStatus('idle');
    setScanResult(null);
    setDeleteResult(null);
    setSelectedPaths(new Set());
    setError(null);
  }, []);

  return {
    status,
    scanResult,
    deleteResult,
    diskInfo,
    selectedPaths,
    error,
    startScan,
    startDelete,
    toggleFileSelection,
    toggleCategorySelection,
    toggleAllSelection,
    refreshDiskInfo,
    clearError,
    clearDeleteResult,
    reset,
  };
}
