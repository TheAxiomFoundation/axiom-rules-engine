// Actual WASM compile/JSON execute boundary, using the shared authoring fixture.
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {createRequire} from 'node:module';
const require=createRequire(import.meta.url);
const engine=require('../pkg-node/axiom_rules_engine_wasm.js');
const target='us:policies/tests/only_related';
const source=readFileSync(new URL('../../tests/fixtures/only_related.yaml',import.meta.url),'utf8');
const artifact=engine.compile(JSON.stringify({[target]:source}),target);
const interval={start:'2026-01-01',end:'2026-01-31'};
function request(targets,value) {
  return {mode:'explain',dataset:{
    inputs:targets.map(id=>({name:`${target}#input.decision`,entity:'Subject',entity_id:id,interval,value:{kind:'bool',value}})),
    relations:targets.map(id=>({name:`${target}#relation.determination_subject`,tuple:['d',id],interval}))},
    queries:[{entity_id:'d',period:{period_kind:'month',...interval},outputs:[`${target}#projected`]}]};
}
for(const mode of ['explain','fast']) {
 for(const value of [false,true]) {
  const req=request(['s'],value); req.mode=mode;
  const result=JSON.parse(engine.execute(artifact,JSON.stringify(req)));
  assert.equal(result.results[0].outputs[`${target}#projected`].value.value,value);
  assert.equal(result.metadata.actual_mode, 'explain');
  if(mode==='fast') assert.match(result.metadata.fallback_reason,/only_related/);
 }
 for(const [ids,error] of [[[],'missing related entity'],[['s1','s2'],'ambiguous relation']]) {
  const req=request(ids,false); req.mode=mode;
  assert.throws(()=>engine.execute(artifact,JSON.stringify(req)),new RegExp(error));
 }
}
console.log('only_related WASM Bool/cardinality checks passed');
