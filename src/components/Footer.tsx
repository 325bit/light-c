// ============================================================================
// Footer copyright notice component
// Reinforces awareness of official channels among users, preventing tampered third-party distribution
// ============================================================================

export function Footer() {
  const currentYear = new Date().getFullYear();

  return (
    <footer className="py-4 px-4 border-t border-[var(--border-color)] bg-[var(--bg-card)]">
      <div className="dashboard-container flex flex-col items-center gap-3">
        {/* Copyright info */}
        <p className="text-[0.79rem] text-[var(--text-muted)]">
          © {currentYear} LightC · All rights reserved.
        </p>

        {/* Official-channel notice */}
        {/* <div className="flex items-center gap-2 px-4 py-2 rounded-lg bg-[var(--bg-hover)]">
          <Shield className="w-3.5 h-3.5 text-[var(--brand-green)]" />
          <p className="text-[0.79rem] text-[var(--text-secondary)]">
            Official release channels:
            <a className="text-[var(--brand-green)] font-medium mx-1" href="https://github.com/Chunyu33/light-c/releases" target="_blank" >GitHub Releases</a>
            ·
            <a className="text-[var(--text-muted)] mx-1" href='https://space.bilibili.com/387797235' target="_blank">Bilibili @Evan's Pixel Space</a>
            · Do not download from third-party sites
          </p>
        </div> */}
      </div>
    </footer>
  );
}

export default Footer;
