// Package the official standalone Node build: a copied Homebrew shim needs libnode.dylib.
const fs=require('node:fs'),path=require('node:path'),crypto=require('node:crypto'),cp=require('node:child_process');
// One entry per packaged target: the vendor archive, and where the binary sits inside it.
const targets={
  'darwin-arm64':{archive:v=>`node-${v}-darwin-arm64.tar.gz`,member:v=>`node-${v}-darwin-arm64/bin/node`,strip:2,binary:'node'},
  'win32-x64':{archive:v=>`node-${v}-win-x64.zip`,member:v=>`node-${v}-win-x64/node.exe`,strip:1,binary:'node.exe'},
};
const target=targets[`${process.platform}-${process.arch}`];
if(!target||Number(process.versions.node.split('.')[0])!==24)throw Error('Package this target with native macOS ARM64 or Windows x64 Node 24');
async function main(){
  const root=path.resolve(__dirname,'../..'), runtime=path.join(root,'ide/.desktop-runtime');
  fs.mkdirSync(runtime,{recursive:true});
  const version=process.version, archive=target.archive(version), base=`https://nodejs.org/dist/${version}/`;
  const sums=await fetch(base+'SHASUMS256.txt');if(!sums.ok)throw Error('Official Node checksums unavailable');
  const expected=(await sums.text()).split('\n').find(line=>line.endsWith('  '+archive))?.split(/\s+/)[0];
  if(!/^[a-f0-9]{64}$/.test(expected||''))throw Error('Official Node archive checksum missing');
  const tarball=path.join(runtime,archive);
  if(!fs.existsSync(tarball)||crypto.createHash('sha256').update(fs.readFileSync(tarball)).digest('hex')!==expected){
    const response=await fetch(base+archive);if(!response.ok)throw Error('Official Node archive unavailable');
    const bytes=Buffer.from(await response.arrayBuffer());
    if(crypto.createHash('sha256').update(bytes).digest('hex')!==expected)throw Error('Official Node archive checksum mismatch');
    fs.writeFileSync(tarball,bytes);
  }
  const temporary=path.join(runtime,'standalone-'+process.pid);fs.mkdirSync(temporary);
  // `-xf` without `-z`: both tars detect gzip, and Windows' bsdtar reads the zip too.
  cp.execFileSync('tar',['-xf',tarball,'-C',temporary,`--strip-components=${target.strip}`,target.member(version)]);
  const binary=path.join(temporary,target.binary);fs.chmodSync(binary,0o755);
  if(cp.execFileSync(binary,['--version'],{encoding:'utf8'}).trim()!==version)throw Error('Standalone Node version mismatch');
  fs.renameSync(binary,path.join(runtime,target.binary));fs.rmdirSync(temporary);
  // What was packaged, and nothing about this machine: paths are first-run setup's.
  fs.writeFileSync(path.join(runtime,'runtime.json'),JSON.stringify({nodeVersion:version,nodeArchiveSha256:expected})+'\n');
}
main().catch(error=>{console.error(error.message);process.exitCode=1;});
