export interface ModuleRenderProps {
  /**
   * Only the current module renders the full DOM; inactive modules keep their React instance and local state,
   * but skip large list/chart rendering, reducing reconciliation and layout cost when switching menus.
   */
  isPageActive?: boolean;
}
