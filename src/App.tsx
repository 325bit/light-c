// ============================================================================
// C drive cleanup tool - main application component
// Single-page dashboard layout, supports light/dark/follow-system themes
// ============================================================================

import { useState, useCallback, useEffect, useRef } from 'react';
import { motion } from 'framer-motion';
import { 
  SettingsModal, 
  TitleBar, 
  ToastProvider, 
  WelcomeModal, 
  shouldShowWelcome,
  UpdateModal,
  DashboardHeader,
  SplashScreen,
  // Footer,
  BackToTopButton,
  AiAssistant,
  SidebarNav,
} from './components';
import { DashboardProvider, useDashboardActions, FontSizeProvider, SettingsProvider, useSettings } from './contexts';
import { APP_MODULES } from './config/modules';
import { useWindowStatePersistence } from './utils/windowState';
import './App.css';

function PageTransitionAccent({ active }: { active: boolean }) {
  if (!active) return null;

  return (
    <motion.div
      // Only animate one lightweight light streak; don't drag the large result DOM into complex transitions, balancing aesthetics and performance.
      className="pointer-events-none absolute inset-x-3 top-0 z-20 h-1 overflow-hidden rounded-full"
      initial={{ opacity: 0 }}
      animate={{ opacity: [0, 1, 0.85, 0] }}
      transition={{ duration: 0.56, ease: 'easeOut' }}
    >
      <motion.div
        className="h-full w-2/3 rounded-full bg-gradient-to-r from-transparent via-[var(--brand-green)] to-transparent shadow-[0_0_18px_rgba(7,193,96,0.65)]"
        initial={{ x: '-130%' }}
        animate={{ x: '170%' }}
        transition={{ duration: 0.56, ease: [0.22, 1, 0.36, 1] }}
      />
    </motion.div>
  );
}

// ============================================================================
// Dashboard content component
// ============================================================================

function DashboardContent() {
  const { triggerOneClickScan } = useDashboardActions();
  const { settings } = useSettings();

  // Settings modal state
  const [showSettings, setShowSettings] = useState(false);
  // Welcome modal state
  const [showWelcome, setShowWelcome] = useState(() => shouldShowWelcome());
  // Module instances stay mounted permanently; switching menus only changes visibility, so scan results and local state are preserved.
  const scrollContainerRef = useRef<HTMLDivElement>(null);
  const [visibleModuleId, setVisibleModuleId] = useState(settings.activeModuleId);
  const [transitionModuleId, setTransitionModuleId] = useState(settings.activeModuleId);
  const [pageTransitionSequence, setPageTransitionSequence] = useState(0);
  const visibleModuleIdRef = useRef(settings.activeModuleId);

  // One-click scan: concurrently starts scans for all modules via the trigger
  const handleOneClickScan = useCallback(() => {
    triggerOneClickScan();
  }, [triggerOneClickScan]);

  useEffect(() => {
    const previousModuleId = visibleModuleIdRef.current;
    if (settings.activeModuleId === previousModuleId) return;

    // Only let the new page do a light entrance animation; hide the old page immediately.
    // Heavy result DOM like big-directory/full-disk analysis: if the old page also played an exit animation, it would trigger large-scale compositing and dropped frames.
    // Scroll back to top before switching, to avoid inheriting the old scrollTop when going from a long page to a short one, which shows big blank areas and extra scrollbars.
    scrollContainerRef.current?.scrollTo({ top: 0, behavior: 'auto' });
    visibleModuleIdRef.current = settings.activeModuleId;
    // Toggle two equivalent class groups by index, so the CSS entrance animation replays reliably without remounting modules.
    setPageTransitionSequence((current) => current + 1);
    setTransitionModuleId(settings.activeModuleId);
    setVisibleModuleId(settings.activeModuleId);
  }, [settings.activeModuleId]);

  return (
    <div
      data-layout-mode="sidebar"
      data-wallpaper-surface
      className="h-screen flex flex-col bg-[var(--bg-base)] overflow-hidden select-none"
    >
      {/* Custom title bar */}
      <TitleBar onSettingsClick={() => setShowSettings(true)} />

      {/* Top statistics bar */}
      <DashboardHeader 
        onOneClickScan={handleOneClickScan}
        onShowWelcome={() => setShowWelcome(true)}
        hideOneClickScan
      />

      {/* Settings modal */}
      <SettingsModal isOpen={showSettings} onClose={() => setShowSettings(false)} />

      {/* Welcome modal */}
      <WelcomeModal isOpen={showWelcome} onClose={() => setShowWelcome(false)} />

      {/* Auto-update check modal */}
      <UpdateModal autoCheck={true} />

      <BackToTopButton scrollContainerRef={scrollContainerRef} />
      {/* AI assistant: a separate overlay added horizontally, not part of any module logic.
          Pass the current module id so the assistant knows which module the user is on.
          The entry can be turned off in "Settings → AI Assistant"; turning it off only hides the UI, providers and API keys are retained. */}
      {settings.aiAssistantEnabled && <AiAssistant activeModuleId={visibleModuleId} />}

      {/* Main content area - module instances stay mounted, but inactive modules skip heavy result DOM themselves. */}
      <main data-wallpaper-surface className="flex-1 min-h-0 overflow-hidden bg-[var(--bg-base)] sidebar-layout">
        <SidebarNav />
        <div className="h-full min-h-0 min-w-0 flex flex-1 flex-col">
          <div ref={scrollContainerRef} className="flex-1 min-h-0 overflow-auto">
            {/* This single-page layer must itself be a flex container: cards use flex-1 to fill the available height,
                if the parent isn't flex the card height is content-driven — modules have different header line counts,
                so the bottom lands differently (Evan reported "some too tall, some too low").
                min-h-full ensures short content still fills the scroll area, long content still scrolls via the outer layer. */}
            <div className="min-h-full box-border flex flex-col relative p-6 dashboard-container module-stage">
              {APP_MODULES.map((moduleConfig) => {
                const ModuleComponent = moduleConfig.component;
                const isActivePage = visibleModuleId === moduleConfig.id;
                const shouldPlayPageEnter = isActivePage && transitionModuleId === moduleConfig.id;
                const pageEnterClass = shouldPlayPageEnter
                  ? pageTransitionSequence % 2 === 0
                    ? 'page-content-enter-even'
                    : 'page-content-enter-odd'
                  : '';
                return (
                  <div
                    key={moduleConfig.id}
                    data-module-id={moduleConfig.id}
                    className={
                      isActivePage
                        // flex-1 + flex-col: pass "fill the remaining height" down,
                        // so the card can fill it; min-height: auto keeps long results from being compressed.
                        ? `relative z-10 overflow-visible flex flex-1 flex-col ${pageEnterClass}`
                        : 'hidden'
                    }
                    style={isActivePage ? { contentVisibility: 'auto' } : undefined}
                  >
                    <PageTransitionAccent active={isActivePage && transitionModuleId === moduleConfig.id} />
                    <ModuleComponent isPageActive={isActivePage} />
                  </div>
                );
              })}
            </div>
          </div>

          {/* Footer isn't put in the scroll area, so short pages don't get extra blank space from the copyright area scrolling. */}
          {/* <Footer /> */}
        </div>
      </main>

    </div>
  );
}

// ============================================================================
// Main application component
// ============================================================================

function App() {
  const [showSplash, setShowSplash] = useState(true);
  const handleSplashComplete = useCallback(() => setShowSplash(false), []);

  // Restore window size during the splash screen, to avoid the layout suddenly changing after the main UI appears.
  useWindowStatePersistence();

  // The splash screen and main UI share the same WebView, avoiding double-window initialization and window-switching overhead.
  if (showSplash) {
    return <SplashScreen onComplete={handleSplashComplete} />;
  }

  // Main window
  return (
    <FontSizeProvider>
      <SettingsProvider>
        <ToastProvider>
          <DashboardProvider>
            <DashboardContent />
          </DashboardProvider>
        </ToastProvider>
      </SettingsProvider>
    </FontSizeProvider>
  );
}

export default App;
