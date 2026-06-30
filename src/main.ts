// Settings window: edit the watched repositories and global settings.
import { invoke } from "@tauri-apps/api/core";

interface RepoConfig {
  id: string;
  name: string;
  github: string;
  local_repo: string;
  worktree_base: string;
  search: string;
  build_commands: string[];
  launch_command: string;
  env: Record<string, string>;
  path_prepend: string[];
  shell: string | null;
}

interface Settings {
  poll_interval_secs: number;
  editor_command: string;
  autostart: boolean;
}

interface Config {
  repos: RepoConfig[];
  settings: Settings;
}

let repos: RepoConfig[] = [];

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

function blankRepo(): RepoConfig {
  return {
    id: "",
    name: "",
    github: "",
    local_repo: "",
    worktree_base: "",
    search: "review-requested:@me",
    build_commands: [],
    launch_command: "",
    env: {},
    path_prepend: [],
    shell: null,
  };
}

function linesToArray(text: string): string[] {
  return text
    .split("\n")
    .map((l) => l.trim())
    .filter((l) => l.length > 0);
}

function envToText(env: Record<string, string>): string {
  return Object.entries(env)
    .map(([k, v]) => `${k}=${v}`)
    .join("\n");
}

function textToEnv(text: string): Record<string, string> {
  const out: Record<string, string> = {};
  for (const line of linesToArray(text)) {
    const idx = line.indexOf("=");
    if (idx > 0) out[line.slice(0, idx).trim()] = line.slice(idx + 1).trim();
  }
  return out;
}

// Project slug from "owner/repo" -> "repo".
function projectName(github: string): string {
  const seg = github.split("/").pop() ?? "";
  return seg.trim();
}

function capitalize(s: string): string {
  return s ? s.charAt(0).toUpperCase() + s.slice(1) : s;
}

// --- Row helpers -----------------------------------------------------------

function makeRow(labelHtml: string, control: HTMLElement, block = false): HTMLElement {
  const row = document.createElement("div");
  row.className = block ? "row block" : "row";
  const label = document.createElement("span");
  label.className = "label";
  label.innerHTML = labelHtml;
  const wrap = document.createElement("span");
  wrap.className = "control";
  wrap.appendChild(control);
  row.append(label, wrap);
  return row;
}

function textInput(value: string, placeholder = "", list?: string): HTMLInputElement {
  const el = document.createElement("input");
  el.type = "text";
  el.value = value;
  el.placeholder = placeholder;
  if (list) el.setAttribute("list", list);
  return el;
}

function textArea(value: string, placeholder = ""): HTMLTextAreaElement {
  const el = document.createElement("textarea");
  el.rows = 3;
  el.value = value;
  el.placeholder = placeholder;
  return el;
}

// --- Repo card -------------------------------------------------------------

function createCard(repo: RepoConfig): HTMLElement {
  const card = document.createElement("div");
  card.className = "card repo";

  // Fields whose value the user has explicitly edited; we stop auto-deriving them.
  const touched = new Set<string>();

  const head = document.createElement("div");
  head.className = "section-head";
  const title = document.createElement("h3");
  title.textContent = repo.name || repo.github || "New repository";
  const remove = document.createElement("button");
  remove.type = "button";
  remove.className = "remove";
  remove.textContent = "Remove";
  remove.addEventListener("click", () => {
    const i = repos.indexOf(repo);
    if (i >= 0) repos.splice(i, 1);
    card.remove();
  });
  head.append(title, remove);
  card.appendChild(head);

  // GitHub picker (also accepts free text), drives the derived fields.
  const githubInput = textInput(repo.github, "owner/repo", "repo-list");
  const nameInput = textInput(repo.name, "My Project");
  const localInput = textInput(repo.local_repo, "~/Projects/my-project");
  const worktreeInput = textInput(repo.worktree_base, "/tmp/my-project-worktrees");

  const refreshTitle = () => {
    title.textContent = repo.name || repo.github || "New repository";
  };

  githubInput.addEventListener("input", () => {
    repo.github = githubInput.value;
    const project = projectName(repo.github);
    if (project) {
      if (!touched.has("name")) {
        repo.name = capitalize(project);
        nameInput.value = repo.name;
      }
      if (!touched.has("local_repo")) {
        repo.local_repo = `~/Projects/${project}`;
        localInput.value = repo.local_repo;
      }
      if (!touched.has("worktree_base")) {
        repo.worktree_base = `/tmp/${project}-worktrees`;
        worktreeInput.value = repo.worktree_base;
      }
    }
    refreshTitle();
  });

  nameInput.addEventListener("input", () => {
    touched.add("name");
    repo.name = nameInput.value;
    refreshTitle();
  });
  localInput.addEventListener("input", () => {
    touched.add("local_repo");
    repo.local_repo = localInput.value;
  });
  worktreeInput.addEventListener("input", () => {
    touched.add("worktree_base");
    repo.worktree_base = worktreeInput.value;
  });

  // Simple text fields bound straight to the repo object.
  const bind = (el: HTMLInputElement, set: (v: string) => void) =>
    el.addEventListener("input", () => set(el.value));

  const searchInput = textInput(repo.search, "review-requested:@me");
  bind(searchInput, (v) => (repo.search = v));
  const launchInput = textInput(repo.launch_command, "./scripts/code.sh");
  bind(launchInput, (v) => (repo.launch_command = v));
  const shellInput = textInput(repo.shell ?? "", "zsh -lc");
  bind(shellInput, (v) => (repo.shell = v.trim() === "" ? null : v.trim()));

  const buildArea = textArea(repo.build_commands.join("\n"), "npm install\nnpm run compile");
  buildArea.addEventListener("input", () => (repo.build_commands = linesToArray(buildArea.value)));
  const pathArea = textArea(repo.path_prepend.join("\n"), "~/.local/share/mise/shims");
  pathArea.addEventListener("input", () => (repo.path_prepend = linesToArray(pathArea.value)));
  const envArea = textArea(envToText(repo.env), "NODE_ENV=development");
  envArea.addEventListener("input", () => (repo.env = textToEnv(envArea.value)));

  card.append(
    makeRow("GitHub (owner/repo)", githubInput),
    makeRow("Display name", nameInput),
    makeRow("Local clone path", localInput),
    makeRow("Worktree base dir", worktreeInput),
    makeRow("Search query", searchInput),
    makeRow("Launch command", launchInput),
    makeRow("Shell override <span class='hint'>(optional)</span>", shellInput),
    makeRow("Build commands <span class='hint'>(one per line)</span>", buildArea, true),
    makeRow("PATH prepend <span class='hint'>(one dir per line)</span>", pathArea, true),
    makeRow("Environment <span class='hint'>(KEY=VALUE per line)</span>", envArea, true),
  );

  return card;
}

function renderRepos() {
  const container = $("repos");
  container.innerHTML = "";
  for (const repo of repos) container.appendChild(createCard(repo));
}

async function loadRepoOptions() {
  try {
    const names = await invoke<string[]>("list_github_repos");
    const list = $("repo-list") as HTMLDataListElement;
    list.innerHTML = "";
    for (const name of names) {
      const opt = document.createElement("option");
      opt.value = name;
      list.appendChild(opt);
    }
  } catch {
    // gh not available / not authed: picker just falls back to free text.
  }
}

async function load() {
  const config = await invoke<Config>("get_config");
  repos = config.repos;
  ($("poll") as HTMLInputElement).value = String(config.settings.poll_interval_secs);
  ($("editor") as HTMLInputElement).value = config.settings.editor_command;
  ($("autostart") as HTMLInputElement).checked = config.settings.autostart;
  renderRepos();
}

async function save() {
  const config: Config = {
    repos,
    settings: {
      poll_interval_secs: Number(($("poll") as HTMLInputElement).value) || 60,
      editor_command: ($("editor") as HTMLInputElement).value,
      autostart: ($("autostart") as HTMLInputElement).checked,
    },
  };
  const status = $("status");
  try {
    await invoke("save_config", { config });
    status.textContent = "Saved ✓";
    setTimeout(() => (status.textContent = ""), 2000);
    await load();
  } catch (err) {
    status.textContent = `Error: ${err}`;
  }
}

window.addEventListener("DOMContentLoaded", () => {
  $("add-repo").addEventListener("click", () => {
    const repo = blankRepo();
    repos.push(repo);
    $("repos").appendChild(createCard(repo));
  });
  $("save").addEventListener("click", save);
  loadRepoOptions();
  load();
});
