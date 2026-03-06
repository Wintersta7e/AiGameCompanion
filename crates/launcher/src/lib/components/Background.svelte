<div class="fixed inset-0 z-0 pointer-events-none overflow-hidden">
  <!-- Blue accent orb -->
  <div
    class="absolute -top-[200px] -right-[200px] w-[800px] h-[800px] rounded-full animate-drift"
    style="background: radial-gradient(circle, rgba(99, 140, 255, 0.06) 0%, transparent 70%);"
  ></div>

  <!-- Purple accent orb -->
  <div
    class="absolute -bottom-[150px] -left-[100px] w-[600px] h-[600px] rounded-full orb-reverse"
    style="background: radial-gradient(circle, rgba(168, 85, 247, 0.05) 0%, transparent 70%);"
  ></div>

  <!-- Scanline overlay -->
  <div
    class="absolute inset-0"
    style="background: repeating-linear-gradient(0deg, transparent, transparent 2px, rgba(0, 0, 0, 0.03) 2px, rgba(0, 0, 0, 0.03) 4px);"
  ></div>
</div>

<style>
  .orb-reverse {
    animation: drift 25s ease-in-out infinite reverse;
  }

  @media (prefers-reduced-motion: reduce) {
    .orb-reverse {
      animation: none !important;
    }
  }
</style>
