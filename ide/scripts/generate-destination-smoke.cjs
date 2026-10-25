const {test} = require('node:test');
const assert = require('node:assert/strict');
const {GenerateService} = require('../gearbox-studio/lib/browser/generate/generate-service');

test('chat and panel planning share destinations without bleeding across products or profiles',async()=>{
 const generate = new GenerateService();
 generate.product = {current:{open:{path:'/workspace/a/product.gdl'},profile:'dev'},revision:1};
 generate.storage = {setData:async()=>{}};
 const calls=[];
 generate.service = {planGenerate:async(...args)=>{calls.push(args);return {out_root:args[2]||'default',plans:[],diagnostics:[]};}};
 await generate.chooseOut('/presentation/a-dev');
 assert.equal((await generate.planFor('/workspace/a/product.gdl','dev')).out_root,'/presentation/a-dev');
 generate.product.current.profile='prod';
 assert.equal((await generate.planFor('/workspace/a/product.gdl','prod')).out_root,'default');
 await generate.chooseOut('/presentation/a-prod');
 await generate.planFor('/workspace/a/product.gdl','prod');
 await generate.planFor('/workspace/a/product.gdl','dev');
 await generate.planFor('/workspace/b/product.gdl','dev');
 await generate.chooseOut(undefined);
 await generate.planFor('/workspace/a/product.gdl','prod');
 assert.deepEqual(calls,[
  ['/workspace/a/product.gdl','dev','/presentation/a-dev'],
  ['/workspace/a/product.gdl','prod',undefined],
  ['/workspace/a/product.gdl','prod','/presentation/a-prod'],
  ['/workspace/a/product.gdl','dev','/presentation/a-dev'],
  ['/workspace/b/product.gdl','dev',undefined],
  ['/workspace/a/product.gdl','prod',undefined],
 ]);
});
