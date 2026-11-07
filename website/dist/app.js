const release = "https://github.com/jaredchu/proclease/releases/download/v0.1.3/";
const command = document.querySelector("#install-command");
const feedback = document.querySelector("#copy-feedback");
const download = document.querySelector("#download");
const checked = name => document.querySelector(`input[name="${name}"]:checked`).value;
function updateCommand() {
  const agent = checked("agent");
  command.textContent = "curl -fsSL https://proclease.com/install.sh | sh" +
    (agent === "none" ? "" : " -s -- --skills " + agent);
}
function updateDownload() {
  download.href = release + "proclease-v0.1.3-" + checked("platform") + ".tar.gz";
}
// Sync with any choice the browser restored before this script ran.
updateCommand();
updateDownload();
document.querySelector("#agent").addEventListener("change", () => {
  updateCommand();
  feedback.textContent = "";
});
document.querySelector("#platform").addEventListener("change", updateDownload);
document.querySelector("#copy-install").addEventListener("click", async () => {
  try {
    await navigator.clipboard.writeText(command.textContent);
    feedback.textContent = "Installation command copied.";
  } catch {
    feedback.textContent = "Copy unavailable. Select the command to copy it manually.";
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
  feedback.textContent = "";
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
advance.addEventListener("click", () => {
  if (expired) {
    document.querySelector("#demo-state").textContent = "● running";
    document.querySelector("#demo-count").textContent = "01 ACTIVE";
    document.querySelector("#demo-time").textContent = "1h 59m";
    document.querySelector("#demo-cpu").textContent = "0.8%";
    document.querySelector("#demo-rss").textContent = "64 MiB";
    document.querySelector("#demo-event").textContent = "Deadline set. No renewal on reads.";
    advance.innerHTML = 'Advance to expiry <span aria-hidden="true">→</span>';
    expired = false;
    return;
  }
  advance.disabled = true;
  document.querySelector("#demo-state").textContent = "● stopping";
  document.querySelector("#demo-time").textContent = "due";
  document.querySelector("#demo-event").textContent = "Deadline reached. Sending TERM to the service group…";
  setTimeout(() => {
    document.querySelector("#demo-state").textContent = "○ expired";
    document.querySelector("#demo-count").textContent = "01 RECENT";
    document.querySelector("#demo-time").textContent = "n/a";
    document.querySelector("#demo-cpu").textContent = "n/a";
    document.querySelector("#demo-rss").textContent = "n/a";
    document.querySelector("#demo-event").textContent = "Service group stopped. History remains available with --all.";
    advance.innerHTML = 'Replay this lease <span aria-hidden="true">↺</span>';
    advance.disabled = false;
    expired = true;
  }, window.matchMedia("(prefers-reduced-motion: reduce)").matches ? 0 : 800);
});
