// ============================================================================
// AI assistant icon (hand-drawn SVG)
//
// Why not use an icon library: the existing Sparkles (four-pointed star) means "shiny / new feature" in this project,
// using it for the AI assistant does not convey "a helper you can chat with" at all.
//
// Drawing notes (trade-offs made for small size):
//   - Must be recognizable at around 20px, so only three pieces of information are kept: a square head + two eyes + one antenna;
//   - Consistent with project icons in using a **stroke** style (not solid blocks), line width 1.75 reads at 24-grid
//     as the same family as Lucide icons;
//   - Uses currentColor, following the text color (currently brand green), so changing themes needs no change here.
// ============================================================================

interface AiMarkIconProps {
  className?: string;
}

export function AiMarkIcon({ className = '' }: AiMarkIconProps) {
  return (
    <svg
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.75"
      strokeLinecap="round"
      strokeLinejoin="round"
      className={className}
      aria-hidden="true"
      focusable="false"
    >
      {/* Antenna: a vertical line + a dot on top, the most economical "robot" signal */}
      <path d="M12 2.6v2.1" />
      <circle cx="12" cy="2" r="1.05" />
      {/* Head: a rounded rectangle, more robot-like than a perfect circle */}
      <rect x="4" y="4.9" width="16" height="13.2" rx="4.2" />
      {/* Eyes: two solid dots, the key to a sense of "aliveness" */}
      <circle cx="9.2" cy="11.2" r="1.15" fill="currentColor" stroke="none" />
      <circle cx="14.8" cy="11.2" r="1.15" fill="currentColor" stroke="none" />
      {/* Mouth: a short line, which together with the eyes forms an expression in pure stroke style */}
      <path d="M9.6 14.3h4.8" />
    </svg>
  );
}
