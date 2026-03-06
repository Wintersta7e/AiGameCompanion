<script lang="ts">
  interface Props {
    gameCount: number;
  }

  let { gameCount }: Props = $props();
</script>

<footer
  class="h-8 flex items-center justify-between px-5 shrink-0 border-t border-border-subtle"
  style="background: rgba(10, 12, 20, 0.9); backdrop-filter: blur(10px);"
>
  <div class="flex items-center gap-4 text-[0.68rem] text-text-muted font-mono tracking-wide">
    <div class="flex items-center gap-[5px]">
      <span
        class="w-1.5 h-1.5 rounded-full bg-accent-tertiary"
        style="box-shadow: 0 0 6px rgba(6, 214, 160, 0.4);"
      ></span>
      <span>Watcher Active</span>
    </div>
    <span>{gameCount} {gameCount === 1 ? "game" : "games"}</span>
  </div>
  <div class="flex items-center gap-4 text-[0.68rem] text-text-muted font-mono tracking-wide">
    <span>v0.5.0</span>
  </div>
</footer>
