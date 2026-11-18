import { type ReactNode } from 'react';
import { createPortal } from 'react-dom';

export type ModuleOperationToolbarId = 'junk' | 'bigFiles' | 'social' | 'context' | 'aiTools' | 'devCache' | 'leftovers';

interface ModuleOperationToolbarProps {
  moduleId: ModuleOperationToolbarId;
  /**
   * Busy state: replace the action buttons with progress content.
   *
   * The action bar is a position: fixed floating layer, so putting progress here **takes no space in document flow** —
   * during deletion the list is neither stretched nor shifted. This is the approach for modules where "users pick a subset to delete"
   * (social-specific cleanup / big files / context menu / registry) need to keep the list as reference but avoid layout jumps.
   */
  busy?: boolean;
  /** Content to show in busy state (usually a progress segment). When busy it replaces children; callers need not change their existing buttons. */
  busyContent?: ReactNode;
  children: ReactNode;
}

/**
 * Batch action bar: fixed at the bottom center of the viewport.
 *
 * Why at the bottom rather than floating on the right: the right side would cover numbers and inline icons in the list,
 * users would have to collapse it first to see content; a horizontal bottom row is the conventional place for "batch actions on selected items",
 * and a horizontal layout takes up no data columns.
 *
 * Why not collapsible: collapsed state leaves only an icon, and most users would not recognize that as the action area —
 * that is worse than "covering content". The cost of collapsing/expanding would be passed on to every use.
 *
 * moduleId only marks which module the action area belongs to (React distinguishes instances by call site), it is not used in rendering.
 */
export function ModuleOperationToolbar({ moduleId: _moduleId, busy = false, busyContent, children }: ModuleOperationToolbarProps) {
  return createPortal(
    <div className={`module-operation-toolbar${busy ? ' module-operation-toolbar--busy' : ''}`}>
      {/* Busy state replaces buttons with ready-made content from the caller. The component itself knows nothing about "deleting" —
          it only places content into the floating layer; wording and business logic are not here, keeping theme-agnostic logic from leaking in. */}
      <div className="module-operation-toolbar__actions">
        {busy ? busyContent : children}
      </div>
    </div>,
    document.body,
  );
}
