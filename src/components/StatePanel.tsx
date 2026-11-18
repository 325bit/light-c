// ============================================================================
// Centered status panel
//
// Does one thing: place a piece of content in the module body that is **centered as a block, with its text left-aligned inside**.
//
// [Why the text isn't centered]
// For 3-4 lines of title + stats + details, the line lengths differ a lot ("Released 67.9 MB successfully, 352.7 MB skipped,
// 42 files will be deleted on reboot" vs "949 succeeded"). With centered text every line has to find its axis again,
// and the eye jumps back and forth; with left alignment the line starts align and it reads like a paragraph. So it's "block centered, text left".
//
// [Who uses it]
// "Deleting" (the block form of ModuleDeleteProgress) and "Cleanup complete" (the result area of ScanSummary).
// These two states are two steps of the same thing, so the geometry must be identical — only the icon and wording change —
// sharing this shell prevents them from drawing their own layouts again (the previous version had one as a rose-red block, the other a 65vh green band,
// three visual languages on the same screen).
// ============================================================================

import type { ReactNode } from 'react';

interface StatePanelProps {
  children: ReactNode;
}

export function StatePanel({ children }: StatePanelProps) {
  return (
    // `module-scan-progress-area` only provides a minimum-height fallback; `flex-1` lets page mode fill the card body,
    // with the content vertically centered inside — don't add another min-height inside to prop up the height.
    <div className="module-scan-progress-area flex flex-1 items-center justify-center p-6">
      {/* Column max width chosen so "one line reads one Chinese sentence"; too wide and long lines scatter */}
      <div className="w-full max-w-[34em]">{children}</div>
    </div>
  );
}

export default StatePanel;
