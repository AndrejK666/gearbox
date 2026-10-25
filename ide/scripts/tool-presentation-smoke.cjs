const {test} = require('node:test');
const assert = require('node:assert/strict');
const React = require('@theia/core/shared/react');
const {renderToStaticMarkup} = require('react-dom/server');
const {ToolSchema} = require('@modelcontextprotocol/sdk/types.js');
const {toolTitle,mcpToolDefinition} = require('../gearbox-studio/lib/common/tool-presentation');
const {TitledToolCard} = require('../gearbox-studio/lib/browser/ai/titled-tool-card');

test('MCP title metadata survives JSON/schema transport without renaming execution identity',()=>{
 const tool={id:'gearbox_get_product',name:'gearbox_get_product',title:'Read product configuration',parameters:{type:'object',properties:{}},handler:async()=>{}};
 const definition=ToolSchema.parse(JSON.parse(JSON.stringify(mcpToolDefinition(tool))));
 assert.equal(definition.name,'gearbox_get_product');
 assert.equal(definition.title,'Read product configuration');
 assert.equal(toolTitle({...tool,title:'',annotations:{title:'Legacy title'}}),'Legacy title');
 assert.equal(toolTitle({...tool,title:undefined}),'gearbox_get_product');
});

test('finished card uses its title and keeps protocol name, args and result in collapsed details',()=>{
 const response={name:'gearbox_get_product',arguments:'{}',finished:true,result:'actual result'};
 const html=renderToStaticMarkup(React.createElement(TitledToolCard,{title:'Read product configuration',response,native:React.createElement('div',{},'native confirmation'),renderResult:()=>React.createElement('p',{},'actual result'),cancelled:false}));
 assert(html.includes('<summary>Read product configuration · Completed</summary>'));
 assert(html.includes('<code>gearbox_get_product</code>'));
 assert(html.includes('actual result'));
 assert(!html.includes('<details open'));
 assert(!html.includes('native confirmation'));
});

test('confirmation remains visible and native while waiting; denial is not displayed as completion',()=>{
 const response={name:'gearbox_toggle_gear',arguments:'{}',finished:false,isAwaitingUserConfirmation:true};
 const props={title:'Change product composition',response,native:React.createElement('button',{},'Allow'),renderResult:()=>null,cancelled:false};
 const waiting=renderToStaticMarkup(React.createElement(TitledToolCard,props));
 assert(waiting.includes('Confirmation required'));
 assert(waiting.includes('<button>Allow</button>'));
 assert(!waiting.includes('hidden='));
 const denied=renderToStaticMarkup(React.createElement(TitledToolCard,{...props,response:{...response,finished:true,result:{denied:true}}}));
 assert(denied.includes('Declined'));
 assert(!denied.includes('Completed'));
});
