// A desk-sized copy of the daemon rules. Index 0 is English, index 1 is Ukrainian.
const names = ["English", "Ukrainian"];
const marks = ["en", "ук"];

const state = {
  nextId: 3,
  focused: 1,
  windows: [
    { id: 1, name: "Firefox", layout: 0 },
    { id: 2, name: "Telegram", layout: 1 },
  ],
};

const windowsEl = document.querySelector("#windows");
const nowEl = document.querySelector("#now");
const keyEn = document.querySelector("#key-en");
const keyUa = document.querySelector("#key-ua");

function focusedWindow() {
  return state.windows.find((window) => window.id === state.focused) ?? null;
}

function shownLayout() {
  const window = focusedWindow();
  return window ? window.layout : 0;
}

function paint() {
  windowsEl.replaceChildren();
  if (state.windows.length === 0 || state.focused === null) {
    const empty = document.createElement("p");
    empty.className = "now";
    empty.textContent = "No window is focused.";
    windowsEl.append(empty);
  } else {
    for (const window of state.windows) {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "window" + (window.id === state.focused ? " on" : "");
      button.setAttribute("aria-pressed", String(window.id === state.focused));
      const title = document.createElement("strong");
      title.textContent = window.name;
      const lang = document.createElement("span");
      lang.className = "lang";
      lang.textContent = marks[window.layout];
      button.append(title, lang);
      button.addEventListener("click", () => {
        state.focused = window.id;
        paint();
      });
      windowsEl.append(button);
    }
  }

  const layout = shownLayout();
  keyEn.classList.toggle("lit", layout === 0);
  keyUa.classList.toggle("lit", layout === 1);
  keyEn.setAttribute("aria-pressed", String(layout === 0));
  keyUa.setAttribute("aria-pressed", String(layout === 1));
  const where = focusedWindow();
  nowEl.textContent = where
    ? `${where.name} is ${names[where.layout]}.`
    : "The empty workspace is English.";
}

function setLayout(index) {
  const window = focusedWindow();
  if (!window) {
    return;
  }
  window.layout = index;
  paint();
}

keyEn.addEventListener("click", () => setLayout(0));
keyUa.addEventListener("click", () => setLayout(1));

document.querySelector("#spawn").addEventListener("click", () => {
  const id = state.nextId;
  state.nextId += 1;
  state.windows.push({ id, name: "Alacritty", layout: 0 });
  state.focused = id;
  paint();
});

document.querySelector("#empty").addEventListener("click", () => {
  state.focused = null;
  paint();
});

paint();
