// ============================================================================
// Module action registry
//
// Confirmation cards are purely presentational; what to delete and how is provided by the module — this table is that interface.
// Same idea as moduleDataRegistry: a module registers an object, the AI layer retrieves it by module id,
// and neither side needs to know what the other's components look like.
//
// Actions are ordered by increasing risk; a module may implement only the earlier ones:
//   selectItems  — only changes selection state (P3a, no risk; the user still clicks Delete through the normal flow)
//   deleteItems  — actually performs deletion (P3b, must go through the module's own safe entry point)
//
// ⚠️ **deleteItems must never implement deletion logic itself**: it must call the module's existing delete function.
//    This way the whole "validate → back up → delete → verify" safety flow still applies,
//    and there is no split where "manual deletion is safe but AI deletion takes a different path".
// ============================================================================

import { useEffect, useRef } from 'react';

/** An item in the deletion target resolution result (for display in the UI). */
export interface AiDeleteTargetSummary {
  /** Stable id, returned as-is in callbacks */
  id: string;
  label: string;
  /** Description such as size and item count, formatted by the module */
  detail?: string;
  /** Irreversible after deletion (like the recycle bin); flagged separately in the UI */
  irreversible?: boolean;
}

/** Deletion execution result. All numbers are provided by the module, **never paraphrased by the model**. */
export interface AiDeleteOutcome {
  deletedCount: number;
  /** Bytes freed; may be omitted if the module cannot compute it */
  freedBytes?: number;
  /** Failure details: name + reason */
  failures: { name: string; reason: string }[];
  /** Whether a reboot is required to take effect */
  needsReboot?: boolean;
}

/**
 * A page of details. **The module provides the content, and the common layer is responsible for saying "this is only part of it"** —
 * the truncation hint is not left to the module's discretion, otherwise it is easily missed and the model thinks it saw everything.
 */
export interface AiReadPage {
  /** One line per entry, formatted by the module itself (path, size, etc.) */
  lines: string[];
  /** Total count, used to tell the model how much of the whole this page covers */
  total: number;
}

export interface ModuleActions {
  /**
   * Resolve a list of ids into a displayable target summary.
   * **Must tolerate stale ids**: data may change between proposal and confirmation (the user selects more, or the module rescans),
   * just drop entries that cannot be resolved; do not throw.
   */
  resolveTargets?: (ids: string[]) => AiDeleteTargetSummary[];
  /** P3a: add targets to the selection state; the user deletes via the original button. */
  selectItems?: (ids: string[]) => void;
  /** P3b: perform deletion. **Must call the module's existing delete entry point**. */
  deleteItems?: (ids: string[]) => Promise<AiDeleteOutcome>;
  /**
   * Read-only: read one page of details under a category.
   * **If not registered, the corresponding tool is not declared** — progressive enhancement, no impact on modules that have not implemented it.
   */
  readItems?: (targetId: string, offset: number, limit: number) => AiReadPage | null;
  /** Read-only: read the full information of a single entry. */
  readItemDetail?: (itemId: string) => string | null;
}

const actions = new Map<string, ModuleActions>();

/** Register a module's actions, returning an unregister function (must be called when the module unmounts, to avoid stale closures). */
export function registerModuleActions(moduleId: string, moduleActions: ModuleActions): () => void {
  actions.set(moduleId, moduleActions);
  return () => {
    // Only delete if we are still the current registration: in page mode a module instance may mount after another unmounts, order is not guaranteed
    if (actions.get(moduleId) === moduleActions) {
      actions.delete(moduleId);
    }
  };
}

/** Get a module's actions; returns null if none registered (the caller uses this to disable buttons on the card). */
export function getModuleActions(moduleId: string | null): ModuleActions | null {
  if (!moduleId) return null;
  return actions.get(moduleId) ?? null;
}

/**
 * Registration hook for use on the module side.
 *
 * A ref holds the latest action object: actions close over the module's current state (selection set, scan result),
 * so it must be "the latest closure, updated every render"; but the registration itself **only needs to happen once**,
 * otherwise every frame would do register + unregister, creating needless side effects.
 * Combining the two: register one stable wrapper function that reads the latest implementation from the ref on each call.
 */
export function useModuleActions(moduleId: string, moduleActions: ModuleActions): void {
  const actionsRef = useRef(moduleActions);
  actionsRef.current = moduleActions;

  useEffect(
    () =>
      registerModuleActions(moduleId, {
        resolveTargets: (ids) => actionsRef.current.resolveTargets?.(ids) ?? [],
        selectItems: (ids) => actionsRef.current.selectItems?.(ids),
        deleteItems: (ids) => {
          const implementation = actionsRef.current.deleteItems;
          // Report an explicit error when the module has no delete capability, instead of silently doing nothing
          if (!implementation) {
            return Promise.reject(new Error('当前模块未提供删除能力'));
          }
          return implementation(ids);
        },
        // ⚠️ Read-only capabilities **must** go through the ref just like the ones above.
        //
        // Writing `readItems: moduleActions.readItems` directly captures the closure **at registration time**,
        // and registration happens only once when moduleId changes — so scanResult in the module stays the initial
        // null, read_items returns empty for **all modules**, and progressive reading is effectively unimplemented.
        // (This is why "the code was written but never worked in tests".)
        readItems: (targetId, offset, limit) =>
          actionsRef.current.readItems?.(targetId, offset, limit) ?? null,
        readItemDetail: (itemId) => actionsRef.current.readItemDetail?.(itemId) ?? null,
      }),
    [moduleId],
  );
}
