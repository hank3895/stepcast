<script>
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";

  const SETTINGS_KEY = "stepcast.settings.v1";
  const defaults = { fps: 12, width: 960, maxSeconds: 60, dither: true, countdown: true, outDir: "" };

  let settings = $state({ ...defaults });
  // idle | countdown | recording | encoding | done | error
  let phase = $state("idle");
  let count = $state(3);
  let elapsed = $state(0);
  let result = $state(null);
  let errorText = $state("");

  let timer = null;
  let countdownTimer = null;

  function loadSettings() {
    try {
      const saved = JSON.parse(localStorage.getItem(SETTINGS_KEY) || "{}");
      settings = { ...defaults, ...saved };
    } catch {
      settings = { ...defaults };
    }
  }

  function saveSettings() {
    try {
      localStorage.setItem(SETTINGS_KEY, JSON.stringify(settings));
    } catch {
      /* storage unavailable: settings last for this session only */
    }
  }

  function stamp() {
    const d = new Date();
    const p = (n) => String(n).padStart(2, "0");
    return `stepcast-${d.getFullYear()}${p(d.getMonth() + 1)}${p(d.getDate())}-${p(d.getHours())}${p(d.getMinutes())}${p(d.getSeconds())}`;
  }

  function fail(e) {
    clearInterval(timer);
    clearInterval(countdownTimer);
    errorText = String(e);
    phase = "error";
  }

  function begin() {
    saveSettings();
    result = null;
    if (settings.countdown) {
      count = 3;
      phase = "countdown";
      countdownTimer = setInterval(() => {
        count -= 1;
        if (count <= 0) {
          clearInterval(countdownTimer);
          startNow();
        }
      }, 1000);
    } else {
      startNow();
    }
  }

  async function startNow() {
    try {
      await invoke("start_recording", { fps: Number(settings.fps), width: Number(settings.width) });
      elapsed = 0;
      phase = "recording";
      const started = Date.now();
      timer = setInterval(() => {
        elapsed = (Date.now() - started) / 1000;
        if (elapsed >= Number(settings.maxSeconds)) stop();
      }, 200);
    } catch (e) {
      fail(e);
    }
  }

  async function stop() {
    if (phase !== "recording") return;
    clearInterval(timer);
    phase = "encoding";
    try {
      result = await invoke("stop_recording", {
        dither: settings.dither,
        outDir: settings.outDir,
        fileName: stamp(),
      });
      phase = "done";
    } catch (e) {
      fail(e);
    }
  }

  function cancelCountdown() {
    clearInterval(countdownTimer);
    phase = "idle";
  }

  function toggle() {
    if (phase === "idle" || phase === "done" || phase === "error") begin();
    else if (phase === "countdown") cancelCountdown();
    else if (phase === "recording") stop();
  }

  async function reveal() {
    try {
      await invoke("reveal_file", { path: result.path });
    } catch (e) {
      errorText = String(e);
    }
  }

  function kb(bytes) {
    return bytes >= 1024 * 1024 ? `${(bytes / 1024 / 1024).toFixed(2)} MB` : `${(bytes / 1024).toFixed(1)} KB`;
  }

  onMount(() => {
    loadSettings();
    if (!settings.outDir) {
      invoke("default_output_dir").then((d) => (settings.outDir = d)).catch(() => {});
    }
    const unlisten = listen("hotkey-toggle", toggle);
    return () => {
      unlisten.then((f) => f());
      clearInterval(timer);
      clearInterval(countdownTimer);
    };
  });
</script>

<main>
  <header>
    <h1>StepCast</h1>
    <span class="hint">F9 開始／停止</span>
  </header>

  {#if phase === "idle" || phase === "error"}
    <section class="panel">
      <div class="grid">
        <label>
          影格率
          <select bind:value={settings.fps}>
            {#each [8, 10, 12, 15, 20, 30] as f}<option value={f}>{f} FPS</option>{/each}
          </select>
        </label>
        <label>
          輸出寬度
          <select bind:value={settings.width}>
            <option value={640}>640 px</option>
            <option value={800}>800 px</option>
            <option value={960}>960 px</option>
            <option value={1280}>1280 px</option>
            <option value={100000}>原始尺寸</option>
          </select>
        </label>
        <label>
          最長錄製
          <select bind:value={settings.maxSeconds}>
            {#each [10, 30, 60, 120, 300] as s}<option value={s}>{s} 秒</option>{/each}
          </select>
        </label>
        <div class="checks">
          <label class="check"><input type="checkbox" bind:checked={settings.countdown} /> 開始前倒數 3 秒</label>
          <label class="check"><input type="checkbox" bind:checked={settings.dither} /> 抖色（漸層較平滑，檔案稍大）</label>
        </div>
      </div>
      <label class="full">
        儲存位置
        <input type="text" bind:value={settings.outDir} spellcheck="false" />
      </label>
      {#if phase === "error"}
        <p class="error">{errorText}</p>
      {/if}
    </section>
    <button class="primary" onclick={begin}>● 開始錄製主螢幕</button>
    <p class="note">錄製時視窗會自動縮到工作列。按 F9 停止，或到達最長錄製時間自動停止。</p>
  {:else if phase === "countdown"}
    <section class="center">
      <div class="big">{count}</div>
      <p>即將開始錄製</p>
      <button class="secondary" onclick={cancelCountdown}>取消</button>
    </section>
  {:else if phase === "recording"}
    <section class="center">
      <div class="rec"><span class="dot"></span> 錄製中 {elapsed.toFixed(1)} 秒</div>
      <button class="primary stop" onclick={stop}>■ 停止</button>
      <p class="note">也可以直接按 F9 停止</p>
    </section>
  {:else if phase === "encoding"}
    <section class="center">
      <div class="spinner"></div>
      <p>正在產生 GIF…</p>
    </section>
  {:else if phase === "done" && result}
    <section class="panel">
      {#if result.preview}
        <img class="preview" src={result.preview} alt="錄製結果預覽" />
      {:else}
        <p class="note">檔案較大，未在此預覽。</p>
      {/if}
      <dl class="stats">
        <dt>檔案大小</dt><dd><strong>{kb(result.file_bytes)}</strong></dd>
        <dt>尺寸</dt><dd>{result.width} × {result.height}</dd>
        <dt>長度</dt><dd>{result.seconds.toFixed(1)} 秒</dd>
        <dt>影格</dt><dd>收到 {result.input_frames} 格，寫入 {result.written_frames} 格</dd>
        <dt>編碼時間</dt><dd>{result.encode_secs.toFixed(2)} 秒</dd>
      </dl>
      <p class="path" title={result.path}>{result.path}</p>
      {#if errorText}<p class="error">{errorText}</p>{/if}
    </section>
    <div class="row">
      <button class="secondary" onclick={reveal}>在資料夾中顯示</button>
      <button class="primary" onclick={begin}>● 再錄一次</button>
    </div>
    <button class="link" onclick={() => (phase = "idle")}>回到設定</button>
  {/if}
</main>

<style>
  main {
    max-width: 560px;
    margin: 0 auto;
    padding: 20px 24px 28px;
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  header {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
  }
  h1 {
    margin: 0;
    font-size: 22px;
    letter-spacing: 0.2px;
  }
  .hint {
    font-size: 12px;
    color: var(--muted);
    border: 1px solid var(--line);
    border-radius: 6px;
    padding: 2px 8px;
  }
  .panel {
    background: var(--card);
    border: 1px solid var(--line);
    border-radius: 12px;
    padding: 16px;
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 12px;
  }
  .checks {
    grid-column: 1 / -1;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  label {
    display: flex;
    flex-direction: column;
    gap: 6px;
    font-size: 13px;
    color: var(--muted);
  }
  label.check {
    flex-direction: row;
    align-items: center;
    gap: 8px;
    color: var(--text);
  }
  select,
  input[type="text"] {
    font: inherit;
    font-size: 14px;
    color: var(--text);
    background: var(--bg);
    border: 1px solid var(--line);
    border-radius: 8px;
    padding: 7px 8px;
  }
  button {
    font: inherit;
    border-radius: 10px;
    border: 1px solid transparent;
    padding: 11px 16px;
    cursor: pointer;
    font-size: 15px;
  }
  .primary {
    background: var(--accent);
    color: white;
    font-weight: 600;
  }
  .primary:hover {
    filter: brightness(1.08);
  }
  .primary.stop {
    background: var(--danger);
    min-width: 160px;
  }
  .secondary {
    background: var(--card);
    color: var(--text);
    border-color: var(--line);
  }
  .link {
    background: none;
    color: var(--muted);
    padding: 4px;
    font-size: 13px;
  }
  .row {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 10px;
  }
  .note {
    margin: 0;
    font-size: 12.5px;
    color: var(--muted);
    line-height: 1.6;
  }
  .error {
    margin: 0;
    color: var(--danger);
    font-size: 13px;
    white-space: pre-wrap;
  }
  .center {
    min-height: 340px;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 16px;
  }
  .big {
    font-size: 96px;
    font-weight: 700;
    color: var(--accent);
    line-height: 1;
  }
  .rec {
    font-size: 20px;
    display: flex;
    align-items: center;
    gap: 10px;
    font-variant-numeric: tabular-nums;
  }
  .dot {
    width: 12px;
    height: 12px;
    border-radius: 50%;
    background: var(--danger);
    animation: blink 1s infinite;
  }
  @keyframes blink {
    50% {
      opacity: 0.25;
    }
  }
  .spinner {
    width: 36px;
    height: 36px;
    border: 3px solid var(--line);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  .preview {
    width: 100%;
    border-radius: 8px;
    border: 1px solid var(--line);
    background: var(--bg);
  }
  .stats {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 6px 14px;
    margin: 0;
    font-size: 13.5px;
  }
  .stats dt {
    color: var(--muted);
  }
  .stats dd {
    margin: 0;
  }
  .path {
    margin: 0;
    font-size: 12px;
    color: var(--muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
