// ============================================================================
// Dashboard state management Context
// Manages scan state for all cleanup modules, supporting concurrent scans and real-time progress updates
// ============================================================================

import { createContext, useContext, useState, useCallback, useEffect, useMemo, useRef, ReactNode } from 'react';
import {
  cancelDiskGrowthScan,
  cancelHotspotScan,
  cancelLargeFileScan,
  getDiskInfo,
  getHealthScore,
  HealthScoreResult,
} from '../api/commands';
import type { DiskInfo } from '../types';

// ============================================================================
// Type definitions
// ============================================================================

/** Module scan status */
export type ModuleStatus = 'idle' | 'scanning' | 'done' | 'error';

/** State of a single module */
export interface ModuleState {
  /** Module status */
  status: ModuleStatus;
  /** Scan progress (0-100) */
  progress: number;
  /** Number of files found */
  fileCount: number;
  /** Total size that can be cleaned */
  totalSize: number;
  /** Error message */
  error: string | null;
  /** Last update time */
  lastUpdated: number;
}

/** Status map for all modules */
export interface ModulesState {
  /** Junk cleanup module */
  junk: ModuleState;
  /** Big file cleanup module */
  bigFiles: ModuleState;
  /** Social software cleanup module */
  social: ModuleState;
  /** System slim module */
  system: ModuleState;
  /** Old driver cleanup module */
  driverCleanup: ModuleState;
  /** Uninstall leftovers module */
  leftovers: ModuleState;
  /** Registry redundancy module */
  registry: ModuleState;
  /** C drive hotspot scan module */
  hotspot: ModuleState;
  /** Context menu cleanup module */
  contextMenu: ModuleState;
  /** C drive full-disk change analysis module */
  diskGrowth: ModuleState;
  /** AI asset analysis module */
  aiModels: ModuleState;
  shellIcons: ModuleState;
}

/** Dashboard Context value type */
export interface DashboardContextValue {
  /** Disk info */
  diskInfo: DiskInfo | null;
  /** Health score data */
  healthData: HealthScoreResult | null;
  /** Whether the health score is loading */
  isLoadingHealth: boolean;
  /** State of all modules */
  modules: ModulesState;
  /** Update module state */
  updateModuleState: (moduleId: keyof ModulesState, state: Partial<ModuleState>) => void;
  /** Refresh disk info */
  refreshDiskInfo: () => Promise<void>;
  /** Refresh health score; pass a drive letter to recompute for that drive, omit to reuse the last drive */
  refreshHealthScore: (driveLetter?: string) => Promise<void>;
  /** Whether any module is scanning */
  isAnyScanning: boolean;
  /** Health score refresh trigger */
  healthRefreshTrigger: number;
  /** Trigger a health score refresh */
  triggerHealthRefresh: () => void;
  /** One-click scan trigger (an incrementing number; modules watch this value to start scanning) */
  oneClickScanTrigger: number;
  /** Trigger a one-click scan */
  triggerOneClickScan: () => void;
  /** Stop all scans */
  stopAllScans: () => void;
  /** Global stop trigger, used by modules to discard async returns from cancelled tasks. */
  stopScanTrigger: number;
}

interface DashboardActionsValue {
  updateModuleState: (moduleId: keyof ModulesState, state: Partial<ModuleState>) => void;
  refreshDiskInfo: () => Promise<void>;
  refreshHealthScore: (driveLetter?: string) => Promise<void>;
  triggerHealthRefresh: () => void;
  triggerOneClickScan: () => void;
  stopAllScans: () => void;
}

interface DashboardSignalsValue {
  healthRefreshTrigger: number;
  oneClickScanTrigger: number;
  stopScanTrigger: number;
}

interface DashboardSummaryValue {
  diskInfo: DiskInfo | null;
  healthData: HealthScoreResult | null;
  isLoadingHealth: boolean;
  isAnyScanning: boolean;
}

// ============================================================================
// Initial state
// ============================================================================

/** Initial state of a module */
const initialModuleState: ModuleState = {
  status: 'idle',
  progress: 0,
  fileCount: 0,
  totalSize: 0,
  error: null,
  lastUpdated: 0,
};

/** Initial state of all modules */
const initialModulesState: ModulesState = {
  junk: { ...initialModuleState },
  bigFiles: { ...initialModuleState },
  social: { ...initialModuleState },
  system: { ...initialModuleState },
  driverCleanup: { ...initialModuleState },
  leftovers: { ...initialModuleState },
  registry: { ...initialModuleState },
  hotspot: { ...initialModuleState },
  contextMenu: { ...initialModuleState },
  diskGrowth: { ...initialModuleState },
  aiModels: { ...initialModuleState },
  shellIcons: { ...initialModuleState },
};

// ============================================================================
// Context creation
// ============================================================================

const DashboardContext = createContext<DashboardContextValue | null>(null);
const DashboardActionsContext = createContext<DashboardActionsValue | null>(null);
const DashboardSignalsContext = createContext<DashboardSignalsValue | null>(null);
const DashboardSummaryContext = createContext<DashboardSummaryValue | null>(null);

const ModuleStateContexts: { [K in keyof ModulesState]: React.Context<ModuleState | null> } = {
  junk: createContext<ModuleState | null>(null),
  bigFiles: createContext<ModuleState | null>(null),
  social: createContext<ModuleState | null>(null),
  system: createContext<ModuleState | null>(null),
  driverCleanup: createContext<ModuleState | null>(null),
  leftovers: createContext<ModuleState | null>(null),
  registry: createContext<ModuleState | null>(null),
  hotspot: createContext<ModuleState | null>(null),
  contextMenu: createContext<ModuleState | null>(null),
  diskGrowth: createContext<ModuleState | null>(null),
  aiModels: createContext<ModuleState | null>(null),
  shellIcons: createContext<ModuleState | null>(null),
};

// ============================================================================
// Provider component
// ============================================================================

interface DashboardProviderProps {
  children: ReactNode;
}

export function DashboardProvider({ children }: DashboardProviderProps) {
  // Disk info
  const [diskInfo, setDiskInfo] = useState<DiskInfo | null>(null);
  // Health score
  const [healthData, setHealthData] = useState<HealthScoreResult | null>(null);
  const [isLoadingHealth, setIsLoadingHealth] = useState(true);
  // Module state
  const [modules, setModules] = useState<ModulesState>(initialModulesState);
  // Health score refresh trigger
  const [healthRefreshTrigger, setHealthRefreshTrigger] = useState(0);
  // One-click scan trigger
  const [oneClickScanTrigger, setOneClickScanTrigger] = useState(0);
  const [stopScanTrigger, setStopScanTrigger] = useState(0);

  // Refresh disk info
  const refreshDiskInfo = useCallback(async () => {
    try {
      const info = await getDiskInfo();
      setDiskInfo(info);
    } catch (error) {
      console.error('获取磁盘信息失败:', error);
    }
  }, []);

  /**
   * Which partition the health score is computed for. A ref is used instead of state because it only determines the request parameters,
   * while the UI reads healthData; the automatic refresh after cleanup must also reuse the drive the user last selected.
   */
  const healthDriveRef = useRef('');
  /** Only the latest request counts when switching drives rapidly, so an older late result does not overwrite the newer one. */
  const healthRequestIdRef = useRef(0);

  // Refresh health score (pass a drive letter to recompute for that drive; omit to reuse the last drive)
  const refreshHealthScore = useCallback(async (driveLetter?: string) => {
    if (driveLetter !== undefined) healthDriveRef.current = driveLetter;
    const requestId = ++healthRequestIdRef.current;
    setIsLoadingHealth(true);
    try {
      const result = await getHealthScore(healthDriveRef.current || undefined);
      if (requestId !== healthRequestIdRef.current) return;
      setHealthData(result);
    } catch (error) {
      if (requestId !== healthRequestIdRef.current) return;
      console.error('获取健康评分失败:', error);
    } finally {
      if (requestId === healthRequestIdRef.current) setIsLoadingHealth(false);
    }
  }, []);

  // Trigger a health score refresh.
  // ⚠️ Also re-read free disk space here (Issue #90): every module calls this after cleanup,
  // while the free space shown in the top-right comes from the snapshot read when this component mounted — without re-reading,
  // you get "space has been freed and Explorer shows it, but the top-right number does not move until the app is restarted".
  const triggerHealthRefresh = useCallback(() => {
    setHealthRefreshTrigger(n => n + 1);
    void getDiskInfo()
      .then(setDiskInfo)
      .catch(error => console.error('刷新磁盘信息失败:', error));
  }, []);

  // Trigger a one-click scan
  const triggerOneClickScan = useCallback(() => {
    setOneClickScanTrigger(n => n + 1);
  }, []);

  // Stop all scans
  const stopAllScans = useCallback(() => {
    // First notify backend tasks that support cancellation, so the UI has stopped but MFT/file scans are still hogging IO in the background.
    void cancelLargeFileScan().catch(error => console.error('停止大文件扫描失败:', error));
    void cancelHotspotScan().catch(error => console.error('停止大目录扫描失败:', error));
    void cancelDiskGrowthScan().catch(error => console.error('停止磁盘变化分析失败:', error));
    setStopScanTrigger(n => n + 1);

    // Reset all scanning modules' state to idle
    setModules(prev => {
      const newModules = { ...prev };
      (Object.keys(newModules) as Array<keyof ModulesState>).forEach(key => {
        if (newModules[key].status === 'scanning') {
          newModules[key] = { ...newModules[key], status: 'idle', progress: 0 };
        }
      });
      return newModules;
    });
  }, []);

  // Update module state
  const updateModuleState = useCallback((moduleId: keyof ModulesState, state: Partial<ModuleState>) => {
    setModules(prev => ({
      ...prev,
      [moduleId]: {
        ...prev[moduleId],
        ...state,
        lastUpdated: Date.now(),
      },
    }));
  }, []);

  // Compute whether any module is scanning
  const isAnyScanning = useMemo(
    () => Object.values(modules).some(m => m.status === 'scanning'),
    [modules]
  );

  // Initial load
  useEffect(() => {
    refreshDiskInfo();
    refreshHealthScore();
  }, [refreshDiskInfo, refreshHealthScore]);

  // Listen for the health score refresh trigger
  useEffect(() => {
    if (healthRefreshTrigger > 0) {
      refreshHealthScore();
      refreshDiskInfo();
    }
  }, [healthRefreshTrigger, refreshHealthScore, refreshDiskInfo]);

  const actionsValue = useMemo<DashboardActionsValue>(() => ({
    updateModuleState,
    refreshDiskInfo,
    refreshHealthScore,
    triggerHealthRefresh,
    triggerOneClickScan,
    stopAllScans,
  }), [
    updateModuleState,
    refreshDiskInfo,
    refreshHealthScore,
    triggerHealthRefresh,
    triggerOneClickScan,
    stopAllScans,
  ]);

  const signalsValue = useMemo<DashboardSignalsValue>(() => ({
    healthRefreshTrigger,
    oneClickScanTrigger,
    stopScanTrigger,
  }), [healthRefreshTrigger, oneClickScanTrigger, stopScanTrigger]);

  const summaryValue = useMemo<DashboardSummaryValue>(() => ({
    diskInfo,
    healthData,
    isLoadingHealth,
    isAnyScanning,
  }), [diskInfo, healthData, isLoadingHealth, isAnyScanning]);

  const value = useMemo<DashboardContextValue>(() => ({
    diskInfo,
    healthData,
    isLoadingHealth,
    modules,
    updateModuleState,
    refreshDiskInfo,
    refreshHealthScore,
    isAnyScanning,
    healthRefreshTrigger,
    triggerHealthRefresh,
    oneClickScanTrigger,
    triggerOneClickScan,
    stopAllScans,
    stopScanTrigger,
  }), [
    diskInfo,
    healthData,
    isLoadingHealth,
    modules,
    updateModuleState,
    refreshDiskInfo,
    refreshHealthScore,
    isAnyScanning,
    healthRefreshTrigger,
    triggerHealthRefresh,
    oneClickScanTrigger,
    triggerOneClickScan,
    stopAllScans,
    stopScanTrigger,
  ]);

  return (
    <DashboardContext.Provider value={value}>
      <DashboardActionsContext.Provider value={actionsValue}>
        <DashboardSignalsContext.Provider value={signalsValue}>
          <DashboardSummaryContext.Provider value={summaryValue}>
            <ModuleStateContexts.junk.Provider value={modules.junk}>
              <ModuleStateContexts.bigFiles.Provider value={modules.bigFiles}>
                <ModuleStateContexts.social.Provider value={modules.social}>
                  <ModuleStateContexts.system.Provider value={modules.system}>
                    <ModuleStateContexts.driverCleanup.Provider value={modules.driverCleanup}>
                      <ModuleStateContexts.leftovers.Provider value={modules.leftovers}>
                      <ModuleStateContexts.registry.Provider value={modules.registry}>
                        <ModuleStateContexts.hotspot.Provider value={modules.hotspot}>
                          <ModuleStateContexts.contextMenu.Provider value={modules.contextMenu}>
                            <ModuleStateContexts.diskGrowth.Provider value={modules.diskGrowth}>
                              <ModuleStateContexts.aiModels.Provider value={modules.aiModels}>
                                <ModuleStateContexts.shellIcons.Provider value={modules.shellIcons}>
                                  {children}
                                </ModuleStateContexts.shellIcons.Provider>
                              </ModuleStateContexts.aiModels.Provider>
                            </ModuleStateContexts.diskGrowth.Provider>
                          </ModuleStateContexts.contextMenu.Provider>
                        </ModuleStateContexts.hotspot.Provider>
                      </ModuleStateContexts.registry.Provider>
                      </ModuleStateContexts.leftovers.Provider>
                    </ModuleStateContexts.driverCleanup.Provider>
                  </ModuleStateContexts.system.Provider>
                </ModuleStateContexts.social.Provider>
              </ModuleStateContexts.bigFiles.Provider>
            </ModuleStateContexts.junk.Provider>
          </DashboardSummaryContext.Provider>
        </DashboardSignalsContext.Provider>
      </DashboardActionsContext.Provider>
    </DashboardContext.Provider>
  );
}

// ============================================================================
// Hook
// ============================================================================

export function useDashboard(): DashboardContextValue {
  const context = useContext(DashboardContext);
  if (!context) {
    throw new Error('useDashboard 必须在 DashboardProvider 内部使用');
  }
  return context;
}

export function useDashboardActions(): DashboardActionsValue {
  const context = useContext(DashboardActionsContext);
  if (!context) {
    throw new Error('useDashboardActions 必须在 DashboardProvider 内部使用');
  }
  return context;
}

export function useDashboardSummary(): DashboardSummaryValue {
  const context = useContext(DashboardSummaryContext);
  if (!context) {
    throw new Error('useDashboardSummary 必须在 DashboardProvider 内部使用');
  }
  return context;
}

export function useDashboardSignals(): DashboardSignalsValue {
  const context = useContext(DashboardSignalsContext);
  if (!context) {
    throw new Error('useDashboardSignals 必须在 DashboardProvider 内部使用');
  }
  return context;
}

export function useDashboardModuleState(moduleId: keyof ModulesState): ModuleState {
  const context = useContext(ModuleStateContexts[moduleId]);
  if (!context) {
    throw new Error('useDashboardModuleState 必须在 DashboardProvider 内部使用');
  }
  return context;
}

export function useModuleDashboard(moduleId: keyof ModulesState) {
  const moduleState = useDashboardModuleState(moduleId);
  const actions = useDashboardActions();
  const signals = useDashboardSignals();

  // Modules subscribe only to their own scan state; global trigger signals are still subscribed separately, keeping one-click scan/stop behavior unchanged.
  return {
    moduleState,
    oneClickScanTrigger: signals.oneClickScanTrigger,
    stopScanTrigger: signals.stopScanTrigger,
    updateModuleState: actions.updateModuleState,
    triggerHealthRefresh: actions.triggerHealthRefresh,
  };
}

export default DashboardContext;
