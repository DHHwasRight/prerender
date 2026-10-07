import { expect, test, type Page } from "@playwright/test";

/**
 * What this example exists to demonstrate: a click replaces only the part of
 * the page below the deepest layer the two pages share.
 *
 * Each test marks live elements, navigates, and checks which marks survived.
 * A mark survives exactly when its element was not replaced, so this measures
 * the swap rather than the markup.
 */

const stamp = (page: Page, selector: string) =>
  page.locator(selector).first().evaluate((el) => el.setAttribute("data-kept", "1"));

const kept = (page: Page, selector: string) =>
  page.locator(`${selector}[data-kept="1"]`).count();

/** The element dp-swap reported replacing. */
function swappedRegion(page: Page) {
  return page.evaluate(
    () =>
      new Promise<string>((resolve) => {
        document.addEventListener(
          "dp:afterSwap",
          (e) => {
            const el = e.target as Element;
            resolve(el.getAttribute("dp-layout") ?? `#${el.id}` ?? el.tagName);
          },
          { once: true },
        );
      }),
  );
}

test("the shell is never re-sent", async ({ page }) => {
  await page.goto("/alpha");
  await stamp(page, ".rail");

  for (const link of ["Beta", "Gamma"]) {
    await page.getByRole("link", { name: link, exact: true }).click();
    await page.waitForURL(new RegExp(link.toLowerCase()));
    expect(await kept(page, ".rail"), `${link} replaced the sidebar`).toBe(1);
  }
});

test("opening a card replaces the board but keeps the section bar", async ({ page }) => {
  await page.goto("/alpha");
  await stamp(page, ".rail");
  await stamp(page, ".bar");
  await stamp(page, ".grid");

  const region = swappedRegion(page);
  await page.getByRole("link", { name: "Alpha 2", exact: true }).click();
  await page.waitForURL(/\/alpha\/card-2/);

  expect(await region, "the swap should land on the section layer").toBe("/alpha");
  expect(await kept(page, ".rail"), "sidebar survives").toBe(1);
  expect(await kept(page, ".bar"), "section bar survives").toBe(1);
  expect(await kept(page, ".grid"), "the grid is replaced").toBe(0);
  await expect(page.locator(".split .detail")).toBeVisible();
  await expect(page.locator(".siblings .sibling")).toHaveCount(5);
});

test("switching panels inside a card replaces only the panel", async ({ page }) => {
  await page.goto("/alpha/card-2/overview");
  await stamp(page, ".rail");
  await stamp(page, ".bar");
  await stamp(page, ".detail-head");
  await stamp(page, ".tabs");
  await stamp(page, ".siblings");

  const region = swappedRegion(page);
  await page.getByRole("link", { name: "Details", exact: true }).click();
  await page.waitForURL(/\/details$/);

  expect(await region, "the swap should land on the card layer").toBe("/alpha/card-2");
  expect(await kept(page, ".detail-head"), "the card header survives").toBe(1);
  expect(await kept(page, ".siblings"), "the sibling list survives").toBe(1);
  expect(await kept(page, ".bar"), "section bar survives").toBe(1);
  // Nothing outside the panel moved, which is the finest swap this site does.
  expect(await kept(page, ".tabs"), "the tab strip survives").toBe(1);
  await expect(page.locator(".panel-label")).toHaveText("Details");
});

test("switching cards replaces the split but not the section bar", async ({ page }) => {
  await page.goto("/alpha/card-2/overview");
  await stamp(page, ".bar");
  await stamp(page, ".detail");

  const region = swappedRegion(page);
  await page.getByRole("link", { name: "Alpha 5", exact: true }).click();
  await page.waitForURL(/\/alpha\/card-5/);

  expect(await region).toBe("/alpha");
  expect(await kept(page, ".bar"), "section bar survives").toBe(1);
  expect(await kept(page, ".detail"), "the old card is gone").toBe(0);
  await expect(page.locator(".detail-title")).toHaveText("Alpha 5");
});

test("closing a card returns to the grid", async ({ page }) => {
  await page.goto("/alpha/card-2/overview");
  await page.locator(".close").click();
  await page.waitForURL(/\/alpha$/);
  await expect(page.locator(".grid .card")).toHaveCount(6);
});

test("every route works with JavaScript off", async ({ browser }) => {
  const context = await browser.newContext({ javaScriptEnabled: false });
  const page = await context.newPage();
  for (const path of ["/alpha", "/alpha/card-2/overview", "/delta/card-6/activity"]) {
    const res = await page.goto(path);
    expect(res?.status(), path).toBe(200);
    await expect(page.locator(".rail")).toBeVisible();
  }
  await context.close();
});
