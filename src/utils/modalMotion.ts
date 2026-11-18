// ============================================================================
// Modal animation presets (framer-motion)
//
// Chinese note: several modals in the project each wrote their own spring parameters (stiffness/damping), hard to tune and easy to
// change one place and miss another. Here we uniformly switch to framer-motion's own semantic spring parameters:
//   - visualDuration: how long the animation "looks" like it takes to reach the target (seconds), more intuitive than stiffness;
//   - bounce: rebound strength, 0 no bounce, 1 very bouncy.
// Note framer-motion's rule: once stiffness/damping/mass are set, visualDuration and bounce
// are ignored, so those three physical parameters no longer appear here.
// ============================================================================

/** Overlay layer: only a opacity fade, to avoid a large blurred layer moving at the same time and producing smear. */
export const MODAL_BACKDROP_MOTION = {
  initial: { opacity: 0 },
  animate: { opacity: 1 },
  exit: { opacity: 0 },
  transition: { duration: 0.2, ease: 'easeOut' },
} as const;

/** Modal card: slight upward move + scale-in on entry, faster and smaller on exit — "appear composed, disappear crisp". */
export const MODAL_CARD_MOTION = {
  initial: { opacity: 0, y: 24, scale: 0.94 },
  animate: { opacity: 1, y: 0, scale: 1 },
  exit: { opacity: 0, y: 10, scale: 0.97 },
  transition: { type: 'spring', visualDuration: 0.32, bounce: 0.2 },
} as const;
