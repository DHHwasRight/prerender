// Fragments live at the page's own URL here; the server picks which tree to
// read from by looking at DP-Partial. The alternative, a `_fragment.html`
// suffix, needs no server support but gives every route two URLs.
dpswap.configure({ fragmentMode: "header" });

// dp-swap's own settling class is added and removed in the same microtask,
// which is right for a one-frame hook and useless for an animation. This marks
// the swapped region until its own animation finishes, so what you see
// highlighted is exactly the subtree that was replaced.
document.addEventListener("dp:afterSwap", (event) => {
  const el = event.target;
  if (!(el instanceof Element)) return;

  el.classList.remove("is-swapped");
  // Reading offsetWidth restarts the animation when the same region is swapped
  // twice in a row; without it the class is already present and nothing runs.
  void el.offsetWidth;
  el.classList.add("is-swapped");

  // animationend bubbles. The content swapped in has entrance animations of
  // its own, and the first of those to finish was ending the highlight early —
  // a 200ms panel fade cut a 700ms ring down to nothing visible. Only this
  // element's own animation counts.
  const done = (e) => {
    if (e.target !== el) return;
    el.classList.remove("is-swapped");
    el.removeEventListener("animationend", done);
  };
  el.addEventListener("animationend", done);
});
