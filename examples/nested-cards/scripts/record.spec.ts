import { test, type Locator, type Page } from "@playwright/test";
import { mkdirSync, renameSync, rmSync } from "node:fs";

/**
 * Walks from the shell down to the deepest layer, pausing on each step long
 * enough for the ring around the swapped region to play out.
 *
 * Playwright draws no pointer in its videos, so one is injected: without it a
 * viewer sees panels change with nothing appearing to cause it.
 */

const SIZE = { width: 1180, height: 560 };
const OUT = "docs/tour.webm";
/** The ring lasts 700ms; this leaves it on screen rather than cutting it off. */
const SETTLE = 1100;

const CURSOR = () => {
  const dot = document.createElement("div");
  dot.style.cssText = `
    position: fixed; top: 0; left: 0; width: 20px; height: 20px; z-index: 2147483647;
    margin: -10px 0 0 -10px; border-radius: 50%; pointer-events: none;
    background: rgba(17,17,17,.18); border: 2px solid #111;
    transition: transform 40ms linear, width 90ms ease, height 90ms ease;
  `;
  const place = () => document.body.appendChild(dot);
  if (document.body) place();
  else document.addEventListener("DOMContentLoaded", place);

  addEventListener("mousemove", (e) => {
    dot.style.transform = `translate(${e.clientX}px, ${e.clientY}px)`;
  });
  // A click with no pointer feedback reads as a jump cut.
  addEventListener("mousedown", () => (dot.style.cssText += "width:12px;height:12px;"));
  addEventListener("mouseup", () => (dot.style.cssText += "width:20px;height:20px;"));
};

/** Moves to the target before clicking it, so the pointer is seen travelling. */
async function tap(page: Page, target: Locator) {
  const box = await target.boundingBox();
  if (!box) throw new Error("target has no box");
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2, { steps: 28 });
  await page.waitForTimeout(220);
  await page.mouse.down();
  await page.waitForTimeout(90);
  await page.mouse.up();
}

test("tour", async ({ browser }) => {
  rmSync("docs/.video", { recursive: true, force: true });
  mkdirSync("docs", { recursive: true });

  const context = await browser.newContext({
    baseURL: "http://127.0.0.1:8097",
    viewport: SIZE,
    recordVideo: { dir: "docs/.video", size: SIZE },
  });
  const page = await context.newPage();
  await page.addInitScript(CURSOR);

  await page.goto("/alpha");
  await page.mouse.move(SIZE.width / 2, SIZE.height / 2);
  await page.waitForTimeout(900);

  // Sidebar: the section below the shell is replaced, the shell is not.
  await tap(page, page.getByRole("link", { name: "Delta", exact: true }));
  await page.waitForURL(/\/delta$/);
  await page.waitForTimeout(SETTLE);

  // A card: the board becomes a split, the section bar stays.
  await tap(page, page.getByRole("link", { name: "Delta 6", exact: true }));
  await page.waitForURL(/\/delta\/card-6/);
  await page.waitForTimeout(SETTLE);

  // The deepest layer: one panel, and a tab strip refreshed out of band.
  await tap(page, page.getByRole("link", { name: "Details", exact: true }));
  await page.waitForURL(/\/details$/);
  await page.waitForTimeout(SETTLE);

  await tap(page, page.getByRole("link", { name: "Activity", exact: true }));
  await page.waitForURL(/\/activity$/);
  await page.waitForTimeout(SETTLE);

  // Back out, to show the same mechanism running in reverse.
  await tap(page, page.locator(".close"));
  await page.waitForURL(/\/delta$/);
  await page.waitForTimeout(SETTLE + 400);

  const video = page.video();
  await context.close();
  if (!video) throw new Error("no video recorded");
  renameSync(await video.path(), OUT);
  rmSync("docs/.video", { recursive: true, force: true });
});
