import { test, expect, type Page } from "@playwright/test";
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { RpcControl } from "../fixtures/rpc";
import { runCommand } from "../fixtures/studio";

async function open(page: Page, id = "ux-workspace") {
  await page.goto("/");
  await page.locator(`[data-start-product*="${id}"]`).first().waitFor({ state: "visible", timeout: 90_000 });
  await page.locator(`[data-start-product*="${id}"]`).first().click();
  await expect(page.locator("[data-product-name]")).toHaveAttribute("data-product-name", id, { timeout: 90_000 });
  await expect(page.locator("[data-workspace-status]")).toHaveAttribute("data-workspace-status", "ready", { timeout: 90_000 });
}
async function section(page: Page, name: string) {
  await page.locator(`[data-product-section="${name}"]`).click();
  await expect(page.locator(`[data-product-section="${name}"]`)).toHaveAttribute("aria-selected", "true");
}

test("one workspace keeps context, keyboard navigation and legacy generation commands", async ({ page }) => {
  await open(page);
  await expect(page.locator(".gbx-product-nav [role=tab]")).toHaveText(["Composition", "Topology", /Checks/, "Generate"]);
  await expect(page.locator("[data-composition-summary]")).toBeVisible();
  await expect(page.getByRole("button", { name: "Remove api-gateway from product" })).not.toBeVisible();
  await expect(page.locator(".gbx-composition-automatic")).not.toHaveAttribute("open");
  const composition = page.locator('[data-product-section="composition"]');
  await composition.focus(); await page.keyboard.press("ArrowRight");
  await expect(page.locator('[data-product-section="topology"]')).toBeFocused();
  await expect(page.locator('[data-product-section="topology"]')).toHaveAttribute("aria-selected", "true");
  await section(page, "generate");
  await expect(page.locator("[data-product-name]")).toHaveText(/UX Workspace/);
  await expect(page.locator("[data-product-profile]")).toHaveValue("dev");
  await expect(page.locator(".gbx-embedded-generate .gbx-generate")).toBeVisible();
  await expect(page.locator('.gbx-widget-generate')).toHaveCount(0);
  await runCommand(page, "Gearbox: Show Generate");
  await expect(page.locator('[data-product-section="generate"]')).toHaveAttribute("aria-selected", "true");
  await page.locator("[data-product-profile]").selectOption("local");
  await expect(page.locator("[data-generate-profile]")).toHaveAttribute("data-generate-profile", "local", { timeout: 60_000 });
});

test("composition search, plugin scopes, draft review and discard stay local", async ({ page }) => {
  await open(page);
  const search = page.getByRole("searchbox", { name: "Search product gears" });
  await search.fill("authentication");
  await expect(page.locator("[data-asked-for]")).toHaveCount(1);
  await search.fill("");
  await page.locator('[data-composition-expand="authn-resolver"]').click();
  await expect(page.locator('[data-plugin-id="static-authn-plugin"]')).toBeVisible();
  await expect(page.locator('[data-plugin-id="oidc-authn-plugin"]')).not.toBeVisible();
  await page.locator(".gbx-inactive-connections > summary").click();
  await expect(page.locator('[data-plugin-id="oidc-authn-plugin"]')).toBeVisible();
  await page.locator('[data-plugin-id="static-authn-plugin"] button').click();
  await expect(page.locator(".gbx-settings-scope")).toContainText("dev");
  const mode = page.locator('[data-config-field="mode"] select');
  await mode.selectOption("static_tokens");
  await expect(page.locator("[data-draft-apply]")).toHaveCount(1);
  await section(page, "generate");
  await expect(page.locator("[data-generation-draft-block]")).toBeVisible();
  await page.locator("[data-draft-discard]").click();
  await expect(page.locator("[data-generation-draft-block]")).toHaveCount(0);
  await section(page, "composition");
  await expect(page.locator('[data-config-field="mode"] select')).toHaveValue("accept_all");
});

test("checks prioritize errors, disclose evidence and retain composition on failure", async ({ page }) => {
  await open(page, "ux-blocked");
  await expect(page.locator("[data-asked-for]").first()).toBeVisible();
  await section(page, "checks");
  await expect(page.locator('[data-conflict-severity="error"]').first()).toBeVisible();
  await expect(page.locator("[data-diagnostic-notes]")).not.toHaveAttribute("open");
  const error = page.locator('[data-conflict-severity="error"]').first();
  await expect(error.locator(".gbx-conflict-help")).not.toBeEmpty();
  await expect(error.locator(".gbx-diagnostic-details")).not.toHaveAttribute("open");
  await error.locator(".gbx-diagnostic-details > summary").click();
  await expect(error.locator(".gbx-drag-handle")).toBeVisible();
  await section(page, "generate");
  await expect(page.locator(".gbx-generate [role=alert]")).toBeVisible();
  await expect(page.getByRole("button", { name: "Generate files", exact: true })).toHaveCount(0);
  await page.getByRole("button", { name: "Review checks", exact: true }).click();
  await expect(page.locator('[data-product-section="checks"]')).toHaveAttribute("aria-selected", "true");
});

for (const width of [1280, 1600]) test(`composition and generation fit ${width}px without horizontal overflow`, async ({ page }) => {
  await page.setViewportSize({ width, height: width === 1280 ? 800 : 1000 });
  await open(page);
  await page.locator('[data-composition-gear="api-gateway"]').click();
  const fits = () => page.locator(".gbx-product").evaluate(el => el.scrollWidth <= el.clientWidth + 1);
  expect(await fits()).toBe(true);
  await page.screenshot({ path: test.info().outputPath("composition.png") });
  await section(page, "generate");
  await expect(page.locator('[data-apply="true"]')).toBeEnabled({ timeout: 60_000 });
  expect(await fits()).toBe(true);
  await page.screenshot({ path: test.info().outputPath("generate.png") });
});

test("add, configure, preview and generate writes only after explicit confirmation", async ({ page }) => {
  await open(page);
  await page.locator("[data-add-gear]").click();
  await page.getByRole("textbox", { name: "Search gear catalogue" }).fill("tenant-resolver");
  await page.locator('[data-add-gear-select="tenant-resolver"]').click();
  await page.locator("[data-add-gear-plugin-pick]").selectOption("static-tr-plugin");
  await page.locator("[data-add-gear-plugin-add]").click();
  await expect(page.locator("[data-add-gear-impact]")).toBeVisible({ timeout: 60_000 });
  const diff = page.locator("[data-add-gear-diff]");
  await expect(diff).not.toBeVisible();
  await page.locator('[data-add-gear-section="closure"] > summary').click();
  await expect(diff).toBeVisible();
  await page.locator("[data-add-gear-submit]").click();
  await expect(page.locator('[data-composition-gear="tenant-resolver"]')).toBeVisible({ timeout: 60_000 });
  await page.locator('[data-composition-expand="authn-resolver"]').click();
  await page.locator('[data-plugin-id="static-authn-plugin"] button').click();
  const mode = page.locator('[data-config-field="mode"] select');
  await mode.selectOption("static_tokens");
  await page.locator("[data-draft-apply]").click();
  await expect(page.locator(".gbx-edit-preview").first()).toBeVisible();
  await page.locator('.gbx-edit-confirm').getByRole("button", { name: "Apply", exact: true }).click();
  await expect(page.locator("[data-draft-apply]")).toHaveCount(0);
  await section(page, "generate");
  await expect(page.locator('[data-apply="true"]')).toBeEnabled({ timeout: 60_000 });
  const outRoot = await page.locator("[data-out-root]").getAttribute("data-out-root");
  const registered = join(outRoot!, "apps", "ux-workspace", "src", "registered_gears.rs");
  expect(existsSync(registered)).toBe(false);
  await page.locator('[data-apply="true"]').click();
  // A file-watcher replan can replace the transient written count immediately.
  // Verify the durable result, including the gear added through the aliased source.
  await expect.poll(() => existsSync(registered) ? readFileSync(registered, "utf8") : "", { timeout: 60_000 }).toContain("tenant_resolver");
});


test("preview refusal is one actionable state without contradictory empty messages", async ({ page, context }) => {
  const rpc = new RpcControl(); await rpc.install(context);
  await open(page);
  const refused = rpc.failNext("applyEdits", "Preview refused for the acceptance test");
  await page.locator("[data-add-gear]").click();
  await page.getByRole("textbox", { name: "Search gear catalogue" }).fill("tenant-resolver");
  await page.locator('[data-add-gear-select="tenant-resolver"]').click();
  await refused.held;
  await expect(page.locator("[data-add-gear-impact-error]")).toBeVisible();
  await expect(page.locator("[data-add-gear-impact]")).toHaveCount(0);
  await expect(page.getByText("Nothing to write yet.", { exact: true })).toHaveCount(0);
  await expect(page.locator("[data-add-gear-submit]")).toBeDisabled();
  refused.release();
  await page.locator("[data-add-gear-cancel]").click();
  await expect(page.locator("[data-add-gear]")).toBeFocused();
});

test("profile resolution and lost engine cannot show a stale generation plan as ready", async ({ page, context }) => {
  const rpc = new RpcControl(); await rpc.install(context);
  await open(page); await section(page, "generate");
  await expect(page.locator("[data-out-root]")).toBeVisible();
  const stalled = rpc.stallNext("resolve");
  await page.locator("[data-product-profile]").selectOption("local");
  await stalled.held;
  await expect(page.locator("[data-workspace-status]")).toContainText("Resolving");
  await expect(page.locator("[data-out-root]")).toHaveCount(0);
  stalled.release();
  await expect(page.locator("[data-generate-profile]")).toHaveAttribute("data-generate-profile", "local");
  const failed = rpc.failNext("resolve", "cannot call resolve: the engine is not initialized");
  await page.locator("[data-product-profile]").selectOption("dev"); await failed.held;
  await expect(page.locator("[data-workspace-status]")).toHaveAttribute("data-workspace-status", "stale");
  await expect(page.locator("[data-reconnect-engine]")).toBeVisible();
  await expect(page.locator('[data-apply="true"]')).toHaveCount(0);
  await section(page, "composition");
  await expect(page.locator("[data-asked-for]").first()).toBeVisible();
  failed.release();
});


test("opening a product does not present a pending composition as empty", async ({ page, context }) => {
  const rpc = new RpcControl(); await rpc.install(context);
  await page.goto("/");
  const product = page.locator('[data-start-product*="ux-blocked"]').first();
  await product.waitFor({ state: "visible", timeout: 90_000 });
  const reading = rpc.stallNext("loadProduct");
  await product.click(); await reading.held;
  await expect(page.getByRole("heading", { name: "Selected gears (0)", exact: true })).toHaveCount(0);
  await expect(page.locator("[data-composition-summary]")).toHaveCount(0);
  const resolving = rpc.stallNext("resolve"); reading.release();
  await resolving.held;
  await expect(page.locator("[data-asked-for]")).toHaveCount(6);
  await expect(page.locator("[data-workspace-status]")).toContainText("Resolving");
  resolving.release();
  await expect(page.locator("[data-workspace-status]")).toHaveAttribute("data-workspace-status", "ready");
});


test("Add Gear preserves the declared source alias when the checkout has another name", async ({ page }) => {
  await open(page, "ux-source-alias");
  await page.locator("[data-add-gear]").click();
  await page.getByRole("textbox", { name: "Search gear catalogue" }).fill("tenant-resolver");
  await page.locator('[data-add-gear-select="tenant-resolver"]').click();
  await expect(page.locator("[data-add-gear-impact]")).toBeVisible();
  await expect(page.locator("[data-add-gear-impact-error]")).toHaveCount(0);
  await page.locator('[data-add-gear-section="closure"] > summary').click();
  await expect(page.locator("[data-add-gear-diff]")).toContainText('source = "gears-rust"');
  await expect(page.locator("[data-add-gear-submit]")).toBeEnabled();
  await page.locator("[data-add-gear-cancel]").click();
  await expect(page.locator('[data-composition-gear="tenant-resolver"]')).toHaveCount(0);
});


test("adding a plugin promotes its implicit host using the product source alias", async ({ page }) => {
  await open(page, "ux-implicit-host");
  await page.locator("[data-add-gear]").click();
  await page.getByRole("textbox", { name: "Search gear catalogue" }).fill("static-authn-plugin");
  await page.locator('[data-add-gear-select="static-authn-plugin"]').click();
  await page.locator("[data-add-gear-host-pick]").selectOption("authn-resolver");
  await expect(page.locator("[data-add-gear-impact]")).toBeVisible();
  await page.locator('[data-add-gear-section="closure"] > summary').click();
  const diff = page.locator("[data-add-gear-diff]");
  await expect(diff).toContainText('use_gear("authn-resolver", source = "gears-rust"');
  await expect(diff).toContainText('static-authn-plugin');
  await page.locator("[data-add-gear-submit]").click();
  await expect(page.locator('[data-composition-gear="authn-resolver"]')).toBeVisible();
});
