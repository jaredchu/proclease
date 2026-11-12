const release = "https://github.com/jaredchu/proclease/releases/download/v0.1.8/";
const command = document.querySelector("#install-command");
const feedback = document.querySelector("#copy-feedback");
const download = document.querySelector("#download");
// Icons are Tabler symbols in index.html; swap the <use> reference and the
// label text separately so neither replaces the other.
const setIcon = (svg, name) => svg.querySelector("use").setAttribute("href", "#i-" + name);
const feedbackIcon = feedback.querySelector("svg");
const feedbackText = feedback.querySelector(".label");
function setFeedback(icon, text) {
  feedbackText.textContent = text;
  // SVG elements have no `hidden` property; toggle the attribute itself.
  feedbackIcon.toggleAttribute("hidden", !icon);
  if (icon) {
    setIcon(feedbackIcon, icon);
    feedback.dataset.state = icon;
  } else {
    delete feedback.dataset.state;
  }
}
const checked = name => document.querySelector(`input[name="${name}"]:checked`).value;
function updateCommand() {
  const agent = checked("agent");
  command.textContent = "curl -fsSL https://proclease.com/install.sh | sh" +
    (agent === "none" ? "" : " -s -- --skills " + agent);
}
function updateDownload() {
  download.href = release + "proclease-v0.1.8-" + checked("platform") + ".tar.gz";
}
// Sync with any choice the browser restored before this script ran.
updateCommand();
updateDownload();
document.querySelector("#agent").addEventListener("change", () => {
  updateCommand();
  setFeedback(null, "");
});
document.querySelector("#platform").addEventListener("change", updateDownload);
document.querySelector("#copy-install").addEventListener("click", async () => {
  try {
    await navigator.clipboard.writeText(command.textContent);
    setFeedback("check", "Installation command copied.");
  } catch {
    setFeedback("alert-circle", "Copy unavailable. Select the command to copy it manually.");
  }
});
const tabs = [...document.querySelectorAll('[role="tab"]')];
function activateTab(tab) {
  for (const item of tabs) {
    const selected = item === tab;
    item.setAttribute("aria-selected", String(selected));
    item.tabIndex = selected ? 0 : -1;
    document.getElementById(item.getAttribute("aria-controls")).hidden = !selected;
  }
  setFeedback(null, "");
}
for (const tab of tabs) {
  tab.addEventListener("click", () => activateTab(tab));
  tab.addEventListener("keydown", event => {
    if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
    event.preventDefault();
    const index = event.key === "Home" ? 0 : event.key === "End" ? tabs.length - 1 :
      (tabs.indexOf(tab) + (event.key === "ArrowRight" ? 1 : -1) + tabs.length) % tabs.length;
    activateTab(tabs[index]);
    tabs[index].focus();
  });
}
let expired = false;
const advance = document.querySelector("#demo-advance");
const stateBadge = document.querySelector("#demo-state");
const text = (selector, value) => { document.querySelector(selector).textContent = value; };
function showState(icon, label) {
  setIcon(stateBadge.querySelector("svg"), icon);
  stateBadge.querySelector(".label").textContent = label;
}
function showControl(icon, label) {
  setIcon(advance.querySelector("svg"), icon);
  advance.querySelector(".label").textContent = label;
}
advance.addEventListener("click", () => {
  if (expired) {
    showState("point-filled", "running");
    text("#demo-count", "01 ACTIVE");
    text("#demo-time", "1h 59m");
    text("#demo-cpu", "0.8%");
    text("#demo-rss", "64 MiB");
    text("#demo-event", "Deadline set. No renewal on reads.");
    showControl("arrow-right", "Advance to expiry");
    expired = false;
    return;
  }
  advance.disabled = true;
  showState("point-filled", "stopping");
  text("#demo-time", "due");
  text("#demo-event", "Deadline reached. Sending TERM to the service group…");
  setTimeout(() => {
    showState("point", "expired");
    text("#demo-count", "01 RECENT");
    text("#demo-time", "n/a");
    text("#demo-cpu", "n/a");
    text("#demo-rss", "n/a");
    text("#demo-event", "Service group stopped. History remains available with --all.");
    showControl("rotate", "Replay this lease");
    advance.disabled = false;
    expired = true;
  }, window.matchMedia("(prefers-reduced-motion: reduce)").matches ? 0 : 800);
});
