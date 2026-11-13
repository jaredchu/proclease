// Keep wide result tables keyboard-scrollable across browser engines.
for (const region of document.querySelectorAll(".table-scroll")) {
  region.addEventListener("keydown", (event) => {
    if (event.target !== region || event.altKey || event.ctrlKey || event.metaKey ||
        region.scrollWidth <= region.clientWidth) return;
    const key = event.key;
    if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(key)) return;
    event.preventDefault();
    if (key === "Home") region.scrollLeft = 0;
    else if (key === "End") region.scrollLeft = region.scrollWidth;
    else region.scrollLeft += key === "ArrowRight" ? 80 : -80;
  });
}
