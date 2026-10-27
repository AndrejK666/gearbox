// Isolated acceptance server: never edits the checkout's products or source corpus.
const fs = require("node:fs"), path = require("node:path"), os = require("node:os");
const { spawn } = require("node:child_process");
const ide = path.resolve(__dirname, "../.."), repo = path.dirname(ide);
const corpus = process.env.GEARBOX_ROOT || path.resolve(repo, "../gears-rust");
const suffix = process.platform === "win32" ? ".exe" : "";
const engine = process.env.GEARBOX_ENGINE || ["debug", "release"].map(mode => path.join(repo, "target", mode, "gearbox" + suffix)).find(p => fs.existsSync(p));
if (!engine || !fs.existsSync(corpus)) throw Error("UX tests need a built engine and GEARBOX_ROOT pointing to the gear corpus.");
const workspace = fs.mkdtempSync(path.join(os.tmpdir(), "gearbox-ux-"));
fs.writeFileSync(path.join(workspace, "Cargo.toml"), "[workspace]\nmembers = []\n");
const originalDemo = fs.readFileSync(path.join(repo, "products/payments-demo/product.gdl"), "utf8")
  .replaceAll("../../../gears-rust", corpus.replaceAll("\\", "/"));
const demo = originalDemo; // A product source alias is independent of its checkout directory name.
for (const [id, name, broken] of [["ux-workspace", "UX Workspace", false], ["ux-blocked", "UX Blocked", true], ["ux-source-alias", "UX Source Alias", false]]) {
  const dir = path.join(workspace, "products", id); fs.mkdirSync(dir, { recursive: true });
  let source = demo.replace('id = "payments-demo"', `id = "${id}"`).replace('name = "Payments Demo"', `name = "${name}"`);
  if (broken) source = source.replace('embedded(development = True', 'embedded(development = False');
  fs.writeFileSync(path.join(dir, "product.gdl"), source);
}
const implicitDir = path.join(workspace, "products", "ux-implicit-host");
fs.mkdirSync(implicitDir, { recursive: true });
fs.writeFileSync(path.join(implicitDir, "product.gdl"), `gdl_version = "1.4"
product(id = "ux-implicit-host", name = "UX Implicit Host", version = "0.1.0",
  sources = [source(id = "gears-rust", at = path(${JSON.stringify(corpus.replaceAll("\\", "/"))}))],
  profiles = [embedded(id = "dev", development = True)], default_profile = "dev",
  gears = [use_gear("api-gateway", source = "gears-rust")])
`);
const child = spawn(process.execPath, [path.join(ide, "browser-app/lib/backend/main.js"), "--hostname=127.0.0.1", "--port=3014"], {
  cwd: ide, stdio: "inherit", windowsHide: true,
  env: { ...process.env, GEARBOX_ENGINE: engine, GEARBOX_ROOT: corpus, GEARBOX_WORKSPACE: workspace, THEIA_CONFIG_DIR: path.join(workspace, "config") },
});
function cleanup() {
  const resolved = path.resolve(workspace), parent = path.resolve(os.tmpdir()) + path.sep;
  if (!resolved.startsWith(parent + "gearbox-ux-")) throw Error("Refusing cleanup outside the UX test workspace");
  fs.rmSync(resolved, { recursive: true, force: true });
}
child.on("exit", code => { cleanup(); process.exit(code || 0); });
for (const signal of ["SIGINT", "SIGTERM"]) process.on(signal, () => child.kill(signal));
