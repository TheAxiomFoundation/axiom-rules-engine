import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {createRequire} from 'node:module';
const require=createRequire(import.meta.url);
const engine=require('../pkg-node/axiom_rules_engine_wasm.js');
const target='ca:policies/tests/selected_year';
const source=readFileSync(new URL('../../tests/fixtures/selected_year_input.yaml',import.meta.url),'utf8');
const artifact=engine.compile(JSON.stringify({[target]:source}),target);
for (const mode of ['explain','fast']) {
 const request={mode,dataset:{inputs:[{name:`${target}#input.income`,entity:'Person',entity_id:'p',interval:{start:'2025-01-01',end:'2025-12-31'},value:{kind:'decimal',value:'1234.567890123456789'}}],relations:[]},queries:[{entity_id:'p',period:{period_kind:'month',start:'2026-07-01',end:'2026-07-31'},outputs:[`${target}#out`]}]};
 const result=JSON.parse(engine.execute(artifact,JSON.stringify(request)));
 assert.equal(result.results[0].outputs[`${target}#out`].value.value,'1234.567890123456789');
 assert.equal(result.metadata.actual_mode,'explain');
 const valid=request.dataset.inputs[0];
 const other={...valid,entity:'Household'};
 for (const inputs of [[valid,other],[other,valid]]) {
  assert.throws(()=>engine.execute(artifact,JSON.stringify({...request,dataset:{...request.dataset,inputs}})),/owner|entity/i);
 }
 request.dataset.inputs[0].interval={start:'2026-01-01',end:'2026-12-31'};
 assert.throws(()=>engine.execute(artifact,JSON.stringify(request)),/missing input/);
}
console.log('Selected annual input WASM source compile, precision and missing-year checks passed');
