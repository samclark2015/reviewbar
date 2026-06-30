// Live build-log viewer: load the existing log, then tail new lines via events.
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

const key = new URLSearchParams(location.search).get("key") ?? "";
const pre = document.getElementById("log") as HTMLPreElement;

function atBottom(): boolean {
  return pre.scrollHeight - pre.scrollTop - pre.clientHeight < 40;
}

function append(text: string) {
  const stick = atBottom();
  pre.textContent += text + "\n";
  if (stick) pre.scrollTop = pre.scrollHeight;
}

async function init() {
  const existing = await invoke<string>("read_log", { key });
  pre.textContent = existing;
  pre.scrollTop = pre.scrollHeight;
  await listen<string>(`log-${key}`, (event) => append(event.payload));
}

init();
