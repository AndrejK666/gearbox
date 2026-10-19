// Package the official standalone Node build: a copied Homebrew shim needs libnode.dylib.
const fs=require('node:fs'),path=require('node:path'),crypto=require('node:crypto'),cp=require('node:child_process');
if(process.platform!=='darwin'||process.arch!=='arm64'||Number(process.versions.node.split('.')[0])!==24)throw Error('Package this target with native macOS ARM64 Node 24');
async function main(){
  const root=path.resolve(__dirname,'../..'), runtime=path.join(root,'ide/.desktop-runtime');
  fs.mkdirSync(runtime,{recursive:true});
  const version=process.version, archive=`node-${version}-darwin-arm64.tar.gz`, base=`https://nodejs.org/dist/${version}/`;
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
  cp.execFileSync('tar',['-xzf',tarball,'-C',temporary,'--strip-components=2',`node-${version}-darwin-arm64/bin/node`]);
  const binary=path.join(temporary,'node');fs.chmodSync(binary,0o755);
  if(cp.execFileSync(binary,['--version'],{encoding:'utf8'}).trim()!==version)throw Error('Standalone Node version mismatch');
  fs.renameSync(binary,path.join(runtime,'node'));fs.rmdirSync(temporary);
  fs.writeFileSync(path.join(runtime,'workspace.json'),JSON.stringify({workspace:fs.realpathSync(root),nodeVersion:version,nodeArchiveSha256:expected})+'\n',{mode:0o600});
}
main().catch(error=>{console.error(error.message);process.exitCode=1;});
