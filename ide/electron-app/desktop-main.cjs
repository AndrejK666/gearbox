const path = require("node:path"), os = require("node:os");
// No machine paths here: the gears checkout and workspace are the person's, and
// first-run setup keeps them in Studio's settings (gearbox.sourceRoots,
// gearbox.workspace). The app carries only what it ships.
const bin = path.join(process.resourcesPath, "bin"), exe = process.platform === "win32" ? ".exe" : "";
process.env.GEARBOX_PACKAGED = "1";
process.env.GEARBOX_ENGINE = path.join(bin, "gearbox" + exe);
process.env.GEARBOX_NODE = path.join(bin, "node" + exe);
// Git ships in resources/plugins; Theia deploys nothing from there unless told.
process.env.THEIA_DEFAULT_PLUGINS = `local-dir:${path.join(process.resourcesPath, "plugins")}`;
const system = process.platform === "darwin" ? ["/opt/homebrew/bin", "/usr/local/bin"] : [];
process.env.PATH = [bin, ...system, path.join(os.homedir(), ".cargo/bin"), process.env.PATH].join(path.delimiter);
process.chdir(os.homedir());
require("./lib/backend/electron-main.js");
