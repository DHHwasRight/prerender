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
  await expect(page.locator(".siblings .sibling")).toHaveCount(6);
  // The open card stays in the list and is marked, rather than vanishing from
  // it — a list that reshuffles under the pointer is hard to use.
  await expect(page.locator(".siblings .sibling.is-current")).toHaveText(/Alpha 2/);
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
  // The tab strip is the one exception, and deliberately so: it sits outside
  // the swapped region but its highlight has to move, so the response carries
  // an out-of-band replacement for it.
  expect(await kept(page, ".tabs"), "the tab strip is replaced out of band").toBe(0);
  await expect(page.locator(".tab.is-current")).toHaveText("Details");
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

test("the server sends less the deeper the shared layer is", async ({ request }) => {
  const url = "/delta/card-6/activity";
  const at = async (depth?: string) =>
    (await (await request.get(url, { headers: depth ? { "DP-Partial": depth } : {} })).text()).length;

  const [full, one, two, three] = await Promise.all([at(), at("1"), at("2"), at("3")]);

  // Each level the client already holds is a level the server does not resend.
  expect(three, "depth 3 should be the smallest").toBeLessThan(two);
  expect(two).toBeLessThan(one);
  expect(one).toBeLessThan(full);
  expect(three, "a panel swap should cost a fraction of the document").toBeLessThan(full * 0.25);
});

test("chrome outside the swap is refreshed out of band", async ({ page }) => {
  await page.goto("/delta/card-6/overview");
  await stamp(page, ".bar");

  await page.getByRole("link", { name: "Activity", exact: true }).click();
  await page.waitForURL(/activity$/);

  // The tab strip is not inside the swapped region, so without an out-of-band
  // replacement it would still be highlighting Overview.
  await expect(page.locator(".tab.is-current")).toHaveText("Activity");
  await expect(page.locator(".panel-label")).toHaveText("Activity");
  expect(await kept(page, ".bar"), "the section bar is still untouched").toBe(1);
});

test("the sidebar highlight follows a section change", async ({ page }) => {
  await page.goto("/alpha");
  await page.getByRole("link", { name: "Gamma", exact: true }).click();
  await page.waitForURL(/\/gamma$/);
  // The sidebar lives outside #app and is never swapped, so this too arrives
  // out of band.
  await expect(page.locator(".rail-link.is-current")).toHaveText("Gamma");
});


test("the highlighted row is tinted with its own card's colour", async ({ page }) => {
  // Two cards whose swatches differ, so a shared accent colour would show up
  // as the same highlight on both.
  for (const [card, swatch] of [
    ["card-6", "#ffafcc"],
    ["card-3", "#a2d2ff"],
  ] as const) {
    await page.goto(`/delta/${card}/overview`);

    const row = page.locator(".sibling.is-current");
    await expect(row).toHaveCount(1);
    await expect(row).toHaveText(new RegExp(card.replace("card-", "Delta ")));

    const [tint, border, dot] = await Promise.all([
      row.evaluate((el) => getComputedStyle(el).getPropertyValue("--tint").trim()),
      row.evaluate((el) => getComputedStyle(el).borderTopColor),
      row.locator(".swatch").evaluate((el) => getComputedStyle(el).backgroundColor),
    ]);

    expect(tint, `${card} row should carry its own colour`).toBe(swatch);
    // The border resolves from the same custom property as the swatch, so the
    // two must agree once the browser has normalised both to rgb().
    expect(border, `${card} border should match its swatch`).toBe(dot);
  }
});
