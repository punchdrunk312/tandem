import "./ghost.css";
import { fire, onSnapshot } from "../bridge";

// A tiny click-through window that Rust keeps under the cursor. This page only decides
// what it shows and tells Rust when to show or hide it.
const FOLDER = `<svg viewBox="0 0 48 48"><path d="M4 12a4 4 0 0 1 4-4h10l4 5h18a4 4 0 0 1 4 4v21a4 4 0 0 1-4 4H8a4 4 0 0 1-4-4z" fill="#f6c453"/><path d="M4 18h40v17a4 4 0 0 1-4 4H8a4 4 0 0 1-4-4z" fill="#ffd978"/></svg>`;
const FILE = `<svg viewBox="0 0 48 48"><path d="M10 4h19l10 10v28a2 2 0 0 1-2 2H10a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2z" fill="#fff" stroke="#b9cad6" stroke-width="1.5"/><path d="M29 4v10h10z" fill="#d9e4ec"/><path d="M14 24h20M14 30h20M14 36h12" stroke="#14b8a6" stroke-width="2.4" stroke-linecap="round"/></svg>`;

const root = document.getElementById("ghost")!;
let shown = false;

onSnapshot((s) => {
  const c = s.carry;
  if (c) {
    const pic = document.createElement("div");
    pic.className = "pic";
    pic.innerHTML = c.folder ? FOLDER : FILE; // static, trusted markup
    if (c.count > 1) {
      const n = document.createElement("span");
      n.className = "count";
      n.textContent = String(c.count);
      pic.append(n);
    }
    const name = document.createElement("div");
    name.className = "name";
    name.textContent = c.name; // file names come from the network: text only
    root.replaceChildren(pic, name);
  }
  if (!!c !== shown) {
    shown = !!c;
    fire("ghost_show", { on: shown });
    requestAnimationFrame(() => root.classList.toggle("on", shown));
  }
});
