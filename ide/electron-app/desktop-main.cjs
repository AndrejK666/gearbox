const fs = require("node:fs"), path = require("node:path");
const config = JSON.parse(fs.readFileSync(path.join(process.resourcesPath, "workspace.json"), "utf8"));
process.env.GEARBOX_WORKSPACE = config.workspace;
process.env.GEARBOX_ENGINE = path.join(process.resourcesPath, "bin/gearbox");
process.env.GEARBOX_NODE = path.join(process.resourcesPath, "bin/node");
process.env.PATH = [path.join(process.resourcesPath,"bin"),"/opt/homebrew/bin","/usr/local/bin",path.join(require("node:os").homedir(),".cargo/bin"),process.env.PATH].join(path.delimiter);
process.chdir(config.workspace);
require("./lib/backend/electron-main.js");
