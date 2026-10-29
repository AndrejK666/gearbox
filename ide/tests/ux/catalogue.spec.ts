import {readFileSync,writeFileSync} from "node:fs";
import {fileURLToPath} from "node:url";
import {resolve,relative,isAbsolute,sep} from "node:path";
import {tmpdir} from "node:os";
import {test,expect,type Page} from "@playwright/test";
import {runCommand} from "../fixtures/studio";

async function open(page:Page,id="ux-workspace") {
 await page.goto("/");const product=page.locator(`[data-start-product*="${id}"]`).first();
 await product.waitFor({state:"visible",timeout:90_000});await product.click();
 await expect(page.locator("[data-workspace-status]")).toHaveAttribute("data-workspace-status","ready",{timeout:90_000});
}
async function catalogue(page:Page){await runCommand(page,"Gearbox Catalogue");await expect(page.locator(".gbx-widget-catalogue")).toBeVisible();}

test("catalogue groups sockets by default, searches descriptions and remembers disclosure state",async({page})=>{
 await open(page);await catalogue(page);const panel=page.locator(".gbx-widget-catalogue");
 const group=panel.locator("[data-group-plugins]");await group.check();
 const socket=panel.locator('[data-catalogue-point]').filter({hasText:"AuthNResolverPluginClient"}).first();
 await expect(socket).toHaveAttribute("aria-expanded","false");
 await expect(panel.locator('[data-row-key$="static-authn-plugin/gear.gdl"]')).toHaveCount(0);
 const search=panel.getByRole("searchbox",{name:"Filter gears"});await search.fill("static-authn-plugin");
 const plugin=panel.locator('.gbx-row').filter({hasText:"Static AuthN Plugin"});
 await expect(plugin).toBeVisible();await expect(plugin).toHaveAttribute("data-catalogue-host","authn-resolver");
 const description=await plugin.locator(".gbx-catalogue-description").textContent();
 await expect(panel.locator("[data-catalogue-count]")).toHaveAttribute("data-catalogue-count","1");
 await expect(socket).toHaveAttribute("aria-expanded","true");
 await search.fill("");await expect(socket).toHaveAttribute("aria-expanded","false");
 await search.fill(description!.split(/\s+/).slice(0,4).join(" "));await expect(plugin).toBeVisible();
});

test("view preference syncs with Add Gear and survives reload without clearing a search or selection",async({page})=>{
 await open(page);await catalogue(page);const panel=page.locator(".gbx-widget-catalogue");
 const search=panel.getByRole("searchbox",{name:"Filter gears"});await search.fill("static-authn-plugin");
 const plugin=panel.locator(".gbx-row").filter({hasText:"Static AuthN Plugin"});await plugin.click();
 const group=panel.locator("[data-group-plugins]");await group.uncheck();
 await expect(search).toHaveValue("static-authn-plugin");await expect(plugin).toHaveClass(/gbx-selected/);
 await expect(plugin).not.toHaveAttribute("data-catalogue-host","authn-resolver");
 await page.locator("[data-add-gear]").click();const dialog=page.locator("[data-add-gear-flow]");
 await expect(dialog.locator("[data-group-plugins]")).not.toBeChecked();
 await dialog.getByRole("textbox",{name:"Search gear catalogue"}).fill("static-authn-plugin");
 const choice=dialog.locator('[data-add-gear-select="static-authn-plugin"]');await choice.click();
 await dialog.locator("[data-group-plugins]").check();
 await expect(choice).toHaveClass(/gbx-selected/);await expect(choice).toBeVisible();
 await expect(dialog.getByRole("textbox",{name:"Search gear catalogue"})).toHaveValue("static-authn-plugin");
 await dialog.locator("[data-group-plugins]").uncheck();await page.locator("[data-add-gear-cancel]").click();
 await page.reload();await page.locator('[data-start-product*="ux-workspace"]').first().click();
 await expect(page.locator("[data-workspace-status]")).toHaveAttribute("data-workspace-status","ready",{timeout:90_000});
 await page.locator("[data-add-gear]").click();await expect(page.locator("[data-add-gear-flow] [data-group-plugins]")).not.toBeChecked();
 await page.locator("[data-add-gear-flow] [data-group-plugins]").check();await page.locator("[data-add-gear-cancel]").click();
});

test("nested plugin previews and confirms one host-plugin pair while Cancel leaves the product empty",async({page})=>{
 await open(page,"ux-pair");await page.locator("[data-add-gear]").click();
 const dialog=page.locator("[data-add-gear-flow]");await dialog.locator("[data-group-plugins]").check();
 await dialog.getByRole("textbox",{name:"Search gear catalogue"}).fill("static-tr-plugin");
 const choice=dialog.locator('[data-add-gear-select="static-tr-plugin"]');
 await expect(choice).toHaveAttribute("data-catalogue-host","tenant-resolver");await choice.click();
 await expect(dialog.locator("[data-add-gear-host-pick]")).toHaveValue("tenant-resolver");
 await expect(dialog.locator("[data-add-gear-impact]")).toBeVisible();
 await expect(dialog.locator("[data-add-gear-promotes-host]")).toContainText("tenant-resolver");
 await dialog.locator('[data-add-gear-section="closure"] > summary').click();
 await expect(dialog.locator("[data-add-gear-diff]")).toContainText('use_gear("tenant-resolver", source = "gears-rust"');
 await expect(dialog.locator("[data-add-gear-diff]")).toContainText('static-tr-plugin');
 await page.locator("[data-add-gear-cancel]").click();await expect(page.locator("[data-asked-for]")).toHaveCount(0);
 await page.locator("[data-add-gear]").click();await dialog.getByRole("textbox",{name:"Search gear catalogue"}).fill("static-tr-plugin");
 await choice.click();await expect(dialog.locator("[data-add-gear-impact]")).toBeVisible();
 await page.locator("[data-add-gear-submit]").click();await expect(page.locator('[data-composition-gear="tenant-resolver"]')).toBeVisible();
 await page.locator('[data-composition-expand="tenant-resolver"]').click();
 await expect(page.locator('[data-plugin-id="static-tr-plugin"]')).toBeVisible();
});

for (const width of [1280,1600]) test(`socket keyboard navigation has one tree tab stop and fits ${width}px`,async({page},testInfo)=>{
 await page.setViewportSize({width,height:width===1280 ? 800 : 1000});await open(page);await page.locator("[data-add-gear]").click();
 const dialog=page.locator("[data-add-gear-flow]");await dialog.locator("[data-group-plugins]").check();
 const tree=dialog.getByRole("tree");await expect(tree.locator('[role="treeitem"][tabindex="0"]')).toHaveCount(1);
 const socket=tree.locator('[data-catalogue-point]').filter({hasText:"AuthNResolverPluginClient"}).first();
 await socket.focus();await page.keyboard.press("ArrowRight");await expect(socket).toHaveAttribute("aria-expanded","true");
 await page.keyboard.press("ArrowRight");const focused=tree.locator(':focus');await expect(focused).toHaveAttribute("data-catalogue-host","authn-resolver");
 await page.keyboard.press("ArrowLeft");await expect(socket).toBeFocused();await page.keyboard.press("ArrowLeft");
 await expect(socket).toHaveAttribute("aria-expanded","false");
 expect(await dialog.evaluate(el=>el.scrollWidth<=el.clientWidth+1)).toBe(true);
 await page.screenshot({path:testInfo.outputPath("catalogue.png")});
 await page.locator("[data-add-gear-cancel]").click();await expect(page.locator("[data-add-gear]")).toBeFocused();
});


test("a stale host-plugin proposal requires another confirmation and preserves the external edit",async({page})=>{
 await open(page,"ux-stale-pair");
 const href=await page.getByRole("link",{name:"UX Stale Pair",exact:true}).getAttribute("href");
 const productPath=fileURLToPath(href!);
 const within=relative(resolve(tmpdir()),resolve(productPath));
 expect(!isAbsolute(within) && !within.startsWith(".."+sep) && within.split(sep)[0]!.startsWith("gearbox-ux-")).toBe(true);
 await page.locator("[data-add-gear]").click();const dialog=page.locator("[data-add-gear-flow]");
 await dialog.locator("[data-group-plugins]").check();
 await dialog.getByRole("textbox",{name:"Search gear catalogue"}).fill("static-tr-plugin");
 await dialog.locator('[data-add-gear-select="static-tr-plugin"]').click();
 await expect(dialog.locator("[data-add-gear-impact]")).toBeVisible();
 const concurrent="# Updated by another author\n"+readFileSync(productPath,"utf8");writeFileSync(productPath,concurrent);
 await page.locator("[data-add-gear-submit]").click();
 await expect(dialog.locator("[data-add-gear-notice]")).toContainText("confirm again");
 expect(readFileSync(productPath,"utf8")).toBe(concurrent);
 await expect(page.locator("[data-add-gear-submit]")).toBeEnabled();
 await page.locator("[data-add-gear-submit]").click();
 await expect(page.locator('[data-composition-gear="tenant-resolver"]')).toBeVisible();
 const written=readFileSync(productPath,"utf8");expect(written).toContain("# Updated by another author");expect(written).toContain("static-tr-plugin");
});
