<!--
  Ambient background. Deliberately quiet — the per-game accent provides the
  colour, so this is just two slow, low-opacity orbs over near-black. No
  scanlines (they read cheap and fight with game cover art).
-->
<div class="fixed inset-0 z-0 pointer-events-none overflow-hidden">
  <div
    class="absolute -top-[220px] -right-[180px] w-[760px] h-[760px] rounded-full"
    style="background: radial-gradient(circle, color-mix(in oklab, var(--accent) 10%, transparent) 0%, transparent 70%); animation: drift 26s ease-in-out infinite;"
  ></div>
  <div
    class="absolute -bottom-[180px] -left-[140px] w-[620px] h-[620px] rounded-full"
    style="background: radial-gradient(circle, rgba(255,255,255,0.025) 0%, transparent 70%); animation: drift 32s ease-in-out infinite reverse;"
  ></div>
</div>

<style>
  @keyframes drift {
    0%,
    100% {
      transform: translate(0, 0) scale(1);
    }
    50% {
      transform: translate(40px, 30px) scale(1.08);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    div {
      animation: none !important;
    }
  }
</style>
